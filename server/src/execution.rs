use anyhow::Result;
use jsonschema::JSONSchema;
use serde_json::Value;
use std::collections::HashMap;
use dirs;
use tokio::sync::broadcast;

use crate::models::{ShManifest, ShKind, ShIO, ShAction, ShRole};
use crate::wasm;
use crate::logger::{Logger};

// Constants
const STARTHUB_API_BASE_URL: &str = "https://api.starthub.so";
const STARTHUB_STORAGE_PATH: &str = "/storage/v1/object/public/artifacts";
const STARTHUB_MANIFEST_FILENAME: &str = "starthub-lock.json";
pub struct ExecutionEngine {
    cache_dir: std::path::PathBuf,
    logger: Logger,
}

impl ExecutionEngine {
    pub fn new() -> Self {
        let cache_dir = dirs::cache_dir()
            .unwrap_or(std::env::temp_dir())
            .join("starthub/oci");
        
        // Ensure the cache directory exists
        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            eprintln!("Warning: Failed to create cache directory {:?}: {}", cache_dir, e);
        }
        
        // Create WebSocket sender internally
        let (ws_sender, _) = broadcast::channel(100);
        
        Self {
            cache_dir,
            logger: Logger::new_with_ws_sender(Some(ws_sender)),
        }
    }

    /// Get the WebSocket sender for external use
    pub fn get_ws_sender(&self) -> Option<broadcast::Sender<String>> {
        self.logger.get_ws_sender()
    }

    fn push_to_execution_buffer(&self, buffer: &mut Vec<String>, step_id: String) {
        if !buffer.contains(&step_id) {
            buffer.push(step_id);
        }
    }

    pub async fn execute_action(&mut self, action_ref: &str, inputs: Vec<Value>) -> Result<Value> {
        self.logger.log_info(&format!("Starting execution of action: {}", action_ref), None);
        
        // Ensure cache directory exists before starting execution.
        // It should already exist, but just in case.
        if let Err(e) = std::fs::create_dir_all(&self.cache_dir) {
            self.logger.log_error(&format!("Failed to create cache directory: {}", e), None);
            return Err(anyhow::anyhow!("Failed to create cache directory: {}", e));
        }
        
        // 1. Build the action tree
        self.logger.log_info("Building action tree...", None);
        
        let root_action = self.build_action_tree(
            action_ref,         // Action reference to download
            None,               // No parent action ID (root)
        ).await?;     
        
        // 1) Instantiate and assign the inputs according to the types specified
        let inputs_to_inject = self.instantiate_io(
            &root_action.inputs,
            &inputs, 
            &root_action.types,
            root_action.role.as_ref())?;
        
        
        // Create a new action with injected inputs (avoiding deep clone)
        let new_root_action = ShAction {
            inputs: inputs_to_inject,
            ..root_action
        };        

        
        self.logger.log_success("Action tree built successfully", Some(&new_root_action.id));

        self.logger.log_info("Executing action tree...", Some(&new_root_action.id));
        let executed_action = self.run_action_tree(&new_root_action).await?;
        
        self.logger.log_success("Action execution completed", Some(&new_root_action.id));

        // Extract outputs from the executed action
        let outputs: Vec<Value> = executed_action.outputs.iter()
            .map(|io| io.value.clone().unwrap_or(Value::Null))
            .collect();

        // Return the outputs directly
        Ok(serde_json::to_value(outputs)?)
    }

    async fn run_action_tree(&mut self, action: &ShAction) -> Result<ShAction> {
        // Base condition.
        if action.kind == "wasm" || action.kind == "docker" {
            println!("running wasm action: {:?}", action.name);
            self.logger.log_info(&format!("Executing {} wasm step: {}", action.kind, action.name), Some(&action.id));

            // Extract values from inputs before serializing
            let input_values: Vec<Value> = if action.name == "sleep" {
                vec![
                    Value::Number(5.into()),
                    Value::String("get_simulator".to_string()),
                    Value::String("1".to_string())
                ]
                // action.inputs.iter()
                // .map(|io| io.value.clone().unwrap_or(Value::Null))
                // .collect()
            } else {
                action.inputs.iter()
                .map(|io| io.value.clone().unwrap_or(Value::Null))
                .collect()
            };

            let result = if action.kind == "wasm" {
                wasm::run_wasm_step(
                    action, 
                    &serde_json::to_value(input_values)?, 
                    &self.cache_dir,
                    &|msg, id| self.logger.log_info(msg, id),
                    &|msg, id| self.logger.log_success(msg, id),
                    &|msg, id| self.logger.log_error(msg, id),
                ).await?
            } else if action.kind == "docker" {
                // TODO: Implement docker step execution
                return Err(anyhow::anyhow!("Docker step execution not implemented"));
            } else {
                return Err(anyhow::anyhow!("Unsupported action kind: {}", action.kind));
            };
            
            self.logger.log_success(&format!("{} step completed: {}", action.kind, action.name), Some(&action.id));
            // Parse the result into a vector of JSON objects
            let json_objects: Vec<Value> = if result.is_empty() {
                Vec::new()
            } else {
                if let Some(first_result) = result.first() {
                    if let Some(array) = first_result.as_array() {
                        // For cast actions, preserve the original values without parsing
                        if action.role.as_ref().map_or(false, |r| r == &ShRole::TypingControl) {
                            array.iter().map(|item| item.clone()).collect()
                        } else {
                            array.iter().map(|item| Self::parse(item.clone())).collect()
                        }
                    } else {
                        // For cast actions, preserve the original value without parsing
                        if action.role.as_ref().map_or(false, |r| r == &ShRole::TypingControl) {
                            vec![first_result.clone()]
                        } else {
                            vec![Self::parse(first_result.clone())]
                        }
                    }
                } else {
                    Vec::new()
                }
            };

            // inject the outputs into the action
            let updated_outputs = self.instantiate_io(
                &action.outputs,
                &json_objects,
                &action.types,
                action.role.as_ref()
            )?;

            let updated_action = ShAction {
                outputs: updated_outputs,
                ..action.clone()
            };
            
            return Ok(updated_action);
        }


        let mut execution_buffer: Vec<String> = Vec::new();

        // Initially, we want to inject the input values into the inputs of the steps.
        // This will help us understand what steps are ready to be executed.
        let steps_with_injected_inputs: HashMap<String, ShAction> = self.resolve_parent_inputs_into_steps(&action.inputs, &action.steps);
        let action_with_inputs_resolved_into_steps = ShAction {
            steps: steps_with_injected_inputs,
            ..action.clone()
        };
        
        // Now that we have injected the input values into the steps wherever it's possible, we
        // also want to find the ready steps for the first iteration. Since it's the
        // first iteration, there is no "current step id" yet.
        let ready_step_ids = self.find_ready_step_ids(&action_with_inputs_resolved_into_steps.steps)?;
        
        // TODO: find a way to make this immutable.
        execution_buffer.extend(ready_step_ids);
        
        // Now we can start the functional execution of steps
        // Using a functional approach with a helper function
        let processed_steps = self.process_steps(
            action_with_inputs_resolved_into_steps.clone(),
            execution_buffer,
        ).await?;

        // The outputs could be coming from the parent inputs or the sibling steps.
        let resolved_outputs = self.resolve_outputs(
            &action.outputs,
            &action.inputs,
            &processed_steps
        )?;

        // Create a new action with resolved outputs
        let updated_action = ShAction {
            steps: processed_steps,
            outputs: self.instantiate_io(&action.outputs, &resolved_outputs, &action.types, action.role.as_ref())?,
            ..action.clone()
        };

        Ok(updated_action)
    }

    async fn process_steps(
        &mut self,
        action: ShAction,
        execution_buffer: Vec<String>,
    ) -> Result<HashMap<String, ShAction>> {
        // Functional execution using a recursive approach
        if execution_buffer.is_empty() {
            Ok(action.steps)
        } else {
            // Get the first step and create a new buffer without it
            let current_step_id = execution_buffer.first().unwrap().clone();

            println!("current_step_id from recursive call: {:?}", current_step_id);
            // We create a new buffer without the first step.
            let mut remaining_buffer = execution_buffer.into_iter().skip(1).collect::<Vec<String>>();
            println!("remaining_buffer: {:#?}", remaining_buffer);
            // Execute the step recursively
            if let Some(step) = action.steps.get(&current_step_id) {
                // Since the step is coming from the execution buffer, it means that
                // it is ready to be executed.

                // We exeute the step recursively
                let executed_step = Box::pin(self.run_action_tree(step)).await?;

                // Once we have executed the step, we want to update the corresponding step
                // in the current action.
                let updated_steps: HashMap<String, ShAction> = action.steps.iter()
                    .map(|(id, step)| {
                        if id == &current_step_id {
                            (id.clone(), executed_step.clone())
                        } else {
                            (id.clone(), step.clone())
                        }
                    })
                    .collect();

                let action_with_udpated_steps = ShAction {
                    steps: updated_steps,
                    ..action.clone()
                };

                // For each sibling, we want to inject the outputs of the step
                // we have just executed into the inputs of the dependent step. This way,
                // we collect all the updated steps and create a new action instance.
                let updated_siblings: HashMap<String, ShAction> = self.resolve_parent_input_or_sibling_output_into_steps(&action_with_udpated_steps.inputs, &action_with_udpated_steps.steps);
                let updated_current_action = ShAction {
                    steps: updated_siblings,
                    ..action_with_udpated_steps.clone()
                };
                
                // If the step we have just executed is a flow control step, we want to
                // find the next step by using the first output of the step we have just executed.
                // if step.role.as_ref().map_or(false, |r| r == &ShRole::FlowControl) {
                //     println!("Found flow control step {:?}", step.name);
                //     if let Some(output) = executed_step.outputs.first() {
                //         if let Some(output_value) = &output.value {
                //             if let Some(output_value_str) = output_value.as_str() {
                //                 let next_step_id = output_value_str;

                //                 println!("next_step_id: {:?}", next_step_id);
                //                  // since steps are a HashMap, we can find the next step by using the next_step_id
                //                 if let Some(_next_step) = action.steps.get(next_step_id) {
                //                     let mut new_execution_buffer = remaining_buffer;
                //                     self.push_to_execution_buffer(&mut new_execution_buffer, next_step_id.to_string());
                //                     println!("updated_current_action: {:#?}", updated_current_action);
                //                     println!("new_execution_buffer: {:#?}", new_execution_buffer);
                //                     // Recursively continue with the updated state
                //                     Box::pin(self.process_steps(updated_current_action, new_execution_buffer)).await 
                //                 } else {
                //                     Box::pin(self.process_steps(action, remaining_buffer)).await
                //                 } 
                //             } else {
                //                 Box::pin(self.process_steps(action, remaining_buffer)).await
                //             }
                //         } else {
                //             Box::pin(self.process_steps(action, remaining_buffer)).await
                //         }
                //     } else {
                //         Box::pin(self.process_steps(action, remaining_buffer)).await
                //     }
                // } else {
                    // Now that we have injected all the possible parent/sibling values into
                    // the other siblings, we find the ready steps that are directly downstream of the
                    // step we have just executed and see if they are ready.
                    let downstream_ready_step_keys = self.find_downstream_ready_steps_keys(
                        &updated_current_action.steps,
                        &current_step_id,
                        &updated_current_action.inputs)?;

                    println!("downstream_ready_step_keys: {:#?}", downstream_ready_step_keys);
                    // Create new buffer by combining remaining steps with new downstream steps
                    let mut new_execution_buffer = remaining_buffer;
                    for step_id in downstream_ready_step_keys {
                        self.push_to_execution_buffer(&mut new_execution_buffer, step_id);
                    }
                    
                    println!("new_execution_buffer: {:#?}", new_execution_buffer);
                    // Recursively continue with the updated state
                    Box::pin(self.process_steps(updated_current_action, new_execution_buffer)).await
                // }
             } else {
                 Box::pin(self.process_steps(action, remaining_buffer)).await
             }
        }
    }

    /// Parses a value to a JSON object or array
    fn parse(value: Value) -> Value {
        match value {
            Value::Object(obj) => {
                let new_obj: serde_json::Map<String, Value> = obj.into_iter()
                    .map(|(k, v)| (k, Self::parse(v)))
                    .collect();
                Value::Object(new_obj)
            },
            Value::Array(arr) => {
                Value::Array(arr.into_iter().map(Self::parse).collect())
            },
            Value::String(s) => {
                // Try to parse as JSON
                if let Ok(parsed) = serde_json::from_str::<Value>(&s) {
                    Self::parse(parsed)
                } else {
                    Value::String(s)
                }
            },
            _ => value
        }
    }

    /// Instantiates and assigns values to IO fields in one operation
    fn instantiate_io(
        &self,
        io_fields: &Vec<ShIO>,
        io_values: &Vec<Value>,
        types: &Option<serde_json::Map<String, Value>>,
        action_role: Option<&ShRole>
    ) -> Result<Vec<ShIO>> {
        let cast_values = self.cast(
            types,
            io_fields,
            io_values,
            action_role
        )?;

        let result = io_fields.iter()
            .enumerate()
            .map(|(index, io_field)| {
                if let Some(resolved_value) = cast_values.get(index) {
                    ShIO {
                        value: Some(resolved_value.clone()),
                        ..io_field.clone()
                    }
                } else {
                    io_field.clone()
                }
            })
            .collect();

        Ok(result)
    }

    /// Casts values to the appropriate type
    fn cast(&self,
        types: &Option<serde_json::Map<String, Value>>, 
        io_definitions: &Vec<ShIO>,
        io_values: &Vec<Value>,
        action_role: Option<&ShRole>) -> Result<Vec<Value>> {
        // Skip schema validation for typing_control actions - return values as-is
        if action_role.map_or(false, |role| role == &ShRole::TypingControl) {
            return Ok(io_values.clone());
        }

        let mut values_to_inject: Vec<Value> = Vec::new();
        for (index, input) in io_definitions.iter().enumerate() {
            // For each input definition, we want to fetch the corresponding input value by index
            // and instantiate the input with the value.
            let value_to_inject = io_values.get(index).unwrap().clone();
            // Handle primitive types
            if input.r#type.as_str() == "string" || 
                input.r#type.as_str() == "bool" ||
                input.r#type.as_str() == "number" ||
                input.r#type.as_str() == "object" {
                values_to_inject.push(value_to_inject);
                continue;
            }

            // Handle non-primitive types
            // Since the type definition has a "type" field, we can find the corresponding type
            // in the types object.
            if let Some(types_map) = types {
                if let Some(type_definition) = types_map.get(&input.r#type) {
                    // Once we have found the type, then we want to 
                    let json_schema = match self.convert_to_json_schema(type_definition) {
                        Ok(schema) => schema,
                        Err(e) => {
                            println!("Failed to convert type definition: {}", e);
                            return Err(anyhow::anyhow!("Failed to convert type definition: {}", e));
                        }
                    };

                    // Compile the JSON schema
                    let compiled_schema = match JSONSchema::compile(&json_schema) {
                        Ok(schema) => schema,
                        Err(e) => {
                            println!("Failed to compile schema for type '{}': {}", input.r#type, e);
                            return Err(anyhow::anyhow!("Failed to compile schema for type '{}': {}", input.r#type, e));
                        }
                    };

                    // Validate the resolved template against the schema
                    if compiled_schema.validate(&value_to_inject).is_ok() {
                        values_to_inject.push(value_to_inject);
                    } else {
                        let error_list: Vec<_> = compiled_schema.validate(&value_to_inject).unwrap_err().collect();
                        println!("value to inject: {:#?}", value_to_inject);
                        println!("input: {:#?}", input);
                        println!("type_definition: {:#?}", type_definition);
                        println!("json_schema: {:#?}", json_schema);
                        println!("compiled_schema: {:#?}", compiled_schema);
                        println!("Value {} is invalid: {:?}", index, error_list);
                        return Err(anyhow::anyhow!("Value {} is invalid: {:?}", index, error_list));
                    }
                } else {
                    // Type definition not found in types map - pass through unchanged
                    values_to_inject.push(value_to_inject);
                }
            } else {
                // No types map provided - pass through unchanged
                values_to_inject.push(value_to_inject);
            }
        }
        Ok(values_to_inject)
    }

    /// Resolves IO definitions using input values and steps context
    fn resolve_from_parent_inputs(
        &self,
        io_definitions: &Vec<ShIO>,
        io_values: &Vec<ShIO>,
    ) -> Option<Vec<Value>> {
        // Extract values from the input values vector
        let values: Vec<Value> = io_values.iter()
            .map(|io| io.value.clone().unwrap_or(Value::Null))
            .collect();

        // For every definition, resolve its template (functional approach)
        let resolved_values: Result<Vec<Value>, ()> = io_definitions.iter()
            .map(|definition| {
                // Resolve the template to get the actual value
                self.interpolate_from_parent_inputs(&definition.template, &values)
                    .map_err(|_| ()) // Convert interpolation errors to () first
                    .and_then(|interpolated_template| {
                        // Check if the resolved template still contains unresolved templates
                        if self.contains_unresolved_templates(&interpolated_template) {
                            Err(()) // Cannot resolve this step yet
                        } else {
                            Ok(interpolated_template)
                        }
                    })
            })
            .collect();

        resolved_values.ok()
    }

    fn resolve_outputs(
        &self,
        output_definitions: &Vec<ShIO>,
        action_inputs: &Vec<ShIO>,
        executed_steps: &HashMap<String, ShAction>
    ) -> Result<Vec<Value>, anyhow::Error> {        
        // Extract values from action inputs
        let input_values: Vec<Value> = action_inputs.iter()
            .map(|io| io.value.clone().unwrap_or(Value::Null))
            .collect();

        // Try to resolve each output using both parent inputs and sibling outputs
        let resolved_outputs: Result<Vec<Value>, anyhow::Error> = output_definitions.iter()
            .map(|output_io| {
                self.interpolate_from_parent_input_or_sibling_output(&output_io.template, &input_values, executed_steps, Some(&output_io.r#type))
                    .and_then(|interpolated_template| {
                        if self.contains_unresolved_templates(&interpolated_template) {
                            Err(anyhow::anyhow!("Unresolved templates in output: {}", output_io.name))
                        } else {
                            Ok(interpolated_template)
                        }
                    })
            })
            .collect();

        resolved_outputs
    }


    fn resolve_parent_input_or_sibling_output_into_steps(
        &self,
        action_inputs: &Vec<ShIO>,
        action_steps: &HashMap<String, ShAction>
    ) -> HashMap<String, ShAction> {
        // Extract values from action inputs
        let input_values: Vec<Value> = action_inputs.iter()
            .map(|io| io.value.clone().unwrap_or(Value::Null))
            .collect();

        action_steps.iter()
            .map(|(step_id, step)| {
                // Try to resolve each step's inputs using both parent inputs and sibling outputs
                let resolved_inputs: Result<Vec<Value>, ()> = step.inputs.iter()
                    .map(|input_io| {
                        self.interpolate_from_parent_input_or_sibling_output(&input_io.template, &input_values, action_steps, Some(&input_io.r#type))
                            .map_err(|_| ())
                            .and_then(|interpolated_template| {
                                if self.contains_unresolved_templates(&interpolated_template) {
                                    Err(())
                                } else {
                                    Ok(interpolated_template)
                                }
                            })
                    })
                    .collect();

                if let Ok(resolved_inputs_to_inject_into_child_step) = resolved_inputs {
                    let inputs_to_inject = self.instantiate_io(
                        &step.inputs, 
                        &resolved_inputs_to_inject_into_child_step,
                        &step.types,
                        step.role.as_ref()).ok();
                    
                    if let Some(inputs_to_inject) = inputs_to_inject {
                        // Create new step with injected inputs
                        let new_step = ShAction {
                            inputs: inputs_to_inject,
                            ..step.clone()
                        };
                        (step_id.clone(), new_step)
                    } else {
                        // Keep original step if injection failed
                        (step_id.clone(), step.clone())
                    }
                } else {
                    // Keep original step if resolution failed
                    (step_id.clone(), step.clone())
                }
            })
            .collect()
    }

    fn interpolate_from_parent_inputs(&self, 
        template: &Value, 
        variables: &Vec<Value>, 
    ) -> Result<Value> {
        // println!("Interpolating from parent inputs: {:#?}", template);
        // println!("Variables: {:#?}", variables);
        match template {
            Value::String(s) => {
                // println!("resolve_template_string: {:#?}", s);
                let resolved = self.interpolate_string_from_parent_input(s, variables)?;
                
                // Check if the resolved string is actually a JSON object/array
                // by trying to parse it as JSON
                match serde_json::from_str::<Value>(&resolved) {
                    Ok(parsed_value) => {
                        // If it's a JSON object or array, return it as-is
                        // If it's a primitive (string, number, boolean, null), 
                        // we need to decide whether to keep it as the primitive or as a string
                        match parsed_value {
                            Value::Object(_) | Value::Array(_) => Ok(parsed_value),
                            _ => {
                                // For primitives, always return the parsed value
                                // This preserves the original type (number, boolean, null) from the JSON
                                Ok(parsed_value)
                            }
                        }
                    },
                    Err(_) => {
                        // Not valid JSON, return as string
                        Ok(Value::String(resolved))
                    }
                }
            },
            Value::Object(obj) => {
                // Recursively resolve object templates
                let mut resolved_obj = serde_json::Map::new();
                for (key, value) in obj {
                    let resolved_value = self.interpolate_from_parent_inputs(value, variables)?;
                    resolved_obj.insert(key.clone(), resolved_value);
                }
                
                Ok(Value::Object(resolved_obj))
            },
            Value::Array(arr) => {
                // println!("resolve_template_array: {:#?}", arr);
                // Recursively resolve array templates
                let mut resolved_arr = Vec::new();
                for item in arr {
                    let resolved_item = self.interpolate_from_parent_inputs(item, variables)?;
                    resolved_arr.push(resolved_item);
                }
                Ok(Value::Array(resolved_arr))
            },
            _ => Ok(template.clone())
        }
    }


    fn interpolate_string_from_parent_input(&self, 
        template: &str, 
        variables: &Vec<Value>
    ) -> Result<String> {
        // Handle {{inputs[index]}} patterns (without jsonpath)
        let inputs_simple_re = regex::Regex::new(r"\{\{inputs\[(\d+)\]\}\}")?;
        let result = inputs_simple_re.captures_iter(template)
            .fold(template.to_string(), |acc, cap| {
                if let Some(index_str) = cap.get(1) {
                    if let Ok(index) = index_str.as_str().parse::<usize>() {
                        if let Some(input_value) = variables.get(index) {
                            let replacement = match input_value {
                                Value::String(s) => s.clone(),
                                _ => input_value.to_string(),
                            };
                            return acc.replace(&cap[0], &replacement);
                        }
                    }
                }
                acc
            });
        
        // Handle {{inputs[index].jsonpath}} patterns
        let inputs_re = regex::Regex::new(r"\{\{inputs\[(\d+)\]\.([^}]+)\}\}")?;
        let result = inputs_re.captures_iter(&result.clone())
            .fold(result, |acc, cap| {
                if let (Some(index_str), Some(jsonpath)) = (cap.get(1), cap.get(2)) {
                    if let Ok(index) = index_str.as_str().parse::<usize>() {
                        if let Some(input_value) = variables.get(index) {
                            if let Ok(resolved_value) = self.evaluate_jsonpath(input_value, jsonpath.as_str()) {
                                let replacement = match resolved_value {
                                    Value::String(s) => s.clone(),
                                    _ => resolved_value.to_string(),
                                };
                                return acc.replace(&cap[0], &replacement);
                            }
                        }
                    }
                }
                acc
            });
        
        Ok(result)
    }

    fn interpolate_string_from_sibling_output(&self, 
        template: &str, 
        executed_steps: &HashMap<String, ShAction>
    ) -> Result<String> {
        // Handle {{steps.step_name.outputs[index]}} patterns (without jsonpath)
        let steps_simple_re = regex::Regex::new(r"\{\{steps\.([^.]+)\.outputs\[(\d+)\]\}\}")?;
        let result = steps_simple_re.captures_iter(template)
            .fold(template.to_string(), |acc, cap| {
                if let (Some(step_name), Some(index_str)) = (cap.get(1), cap.get(2)) {
                    if let Ok(index) = index_str.as_str().parse::<usize>() {
                        if let Some(step) = executed_steps.get(step_name.as_str()) {
                            if let Some(output) = step.outputs.get(index) {
                                if let Some(output_value) = &output.value {
                                    let replacement = match output_value {
                                        Value::String(s) => s.clone(),
                                        _ => output_value.to_string(),
                                    };
                                    return acc.replace(&cap[0], &replacement);
                                } else {
                                    println!("DEBUG: No output value found for step: {}", step_name.as_str());
                                }
                            } else {
                                println!("DEBUG: No output found at index {} for step: {}", index, step_name.as_str());
                            }
                        } else {
                            println!("DEBUG: Step not found in executed_steps: {}", step_name.as_str());
                        }
                    }
                }
                acc
            });
        
        // Handle {{steps.step_name.outputs[index].jsonpath}} patterns
        let steps_re = regex::Regex::new(r"\{\{steps\.([^.]+)\.outputs\[(\d+)\]\.([^}]+)\}\}")?;
        let result = steps_re.captures_iter(&result.clone())
            .fold(result, |acc, cap| {
                if let (Some(step_name), Some(index_str), Some(jsonpath)) = (cap.get(1), cap.get(2), cap.get(3)) {
                    if let Ok(index) = index_str.as_str().parse::<usize>() {
                        if let Some(step) = executed_steps.get(step_name.as_str()) {
                            if let Some(output) = step.outputs.get(index) {
                                if let Some(output_value) = &output.value {
                                    if let Ok(resolved_value) = self.evaluate_jsonpath(output_value, jsonpath.as_str()) {
                                        let replacement = match resolved_value {
                                            Value::String(s) => s.clone(),
                                            _ => resolved_value.to_string(),
                                        };
                                        return acc.replace(&cap[0], &replacement);
                                    } else {
                                        println!("DEBUG: Failed to evaluate jsonpath: {}", jsonpath.as_str());
                                    }
                                } else {
                                    println!("DEBUG: No output value found for step: {}", step_name.as_str());
                                }
                            } else {
                                println!("DEBUG: No output found at index {} for step: {}", index, step_name.as_str());
                            }
                        } else {
                            println!("DEBUG: Step not found in executed_steps: {}", step_name.as_str());
                        }
                    }
                }
                acc
            });
        
        Ok(result)
    }

    fn interpolate_string_from_parent_input_or_sibling_output(&self, 
        template: &str, 
        variables: &Vec<Value>,
        executed_steps: &HashMap<String, ShAction>
    ) -> Result<String> {
        // First resolve parent inputs
        let parent_resolved = self.interpolate_string_from_parent_input(template, variables)?;
        
        // Then resolve sibling outputs
        let fully_resolved = self.interpolate_string_from_sibling_output(&parent_resolved, executed_steps)?;
        
        Ok(fully_resolved)
    }

    fn interpolate_from_parent_input_or_sibling_output(&self, 
        template: &Value, 
        variables: &Vec<Value>,
        executed_steps: &HashMap<String, ShAction>,
        type_field: Option<&str>
    ) -> Result<Value> {
        match template {
            Value::String(s) => {
                let resolved = self.interpolate_string_from_parent_input_or_sibling_output(s, variables, executed_steps)?;
                
                // if type_field == Some("any") {
                //     println!("DEBUG: Resolved string is of type any: {}", resolved);
                //     return Ok(Value::String(resolved));
                // }

                // Check if the resolved string is actually a JSON object/array
                // by trying to parse it as JSON
                match serde_json::from_str::<Value>(&resolved) {
                    Ok(parsed_value) => {
                        // If it's a JSON object or array, return it as-is
                        // If it's a primitive (string, number, boolean, null), 
                        // we need to decide whether to keep it as the primitive or as a string
                        match parsed_value {
                            Value::Object(_) | Value::Array(_) => Ok(parsed_value),
                            _ => {
                                // For primitives, always return the parsed value
                                // This preserves the original type (number, boolean, null) from the JSON
                                Ok(parsed_value)
                            }
                        }
                    },
                    Err(_) => {
                        // Not valid JSON, return as string
                        Ok(Value::String(resolved))
                    }
                }
            },
            Value::Object(obj) => {
                // Recursively resolve object templates
                let mut resolved_obj = serde_json::Map::new();
                for (key, value) in obj {
                    let resolved_value = self.interpolate_from_parent_input_or_sibling_output(value, variables, executed_steps, type_field)?;
                    resolved_obj.insert(key.to_string(), resolved_value);
                }
                Ok(Value::Object(resolved_obj))
            },
            Value::Array(arr) => {
                // Recursively resolve array templates
                let resolved_arr: Result<Vec<Value>> = arr.iter()
                    .map(|item| self.interpolate_from_parent_input_or_sibling_output(item, variables, executed_steps, type_field))
                    .collect();
                Ok(Value::Array(resolved_arr?))
            },
            _ => Ok(template.clone())
        }
    }

    fn evaluate_jsonpath(&self, value: &Value, jsonpath: &str) -> Result<Value> {
        // Handle empty path - return the original value
        if jsonpath.is_empty() {
            return Ok(value.clone());
        }
        
        // Simple JSONPath evaluation for common patterns
        let path_parts: Vec<&str> = jsonpath.split('.').collect();
        let mut current = value;
        for part in path_parts {
            // Skip empty parts (e.g., from "a..b" or ".b")
            if part.is_empty() {
                continue;
            }
            // Check if part contains array index like "body[0]"
            if part.contains('[') && part.contains(']') {
                let bracket_start = part.find('[').unwrap();
                let bracket_end = part.find(']').unwrap();
                let key = &part[..bracket_start];
                let index_str = &part[bracket_start + 1..bracket_end];
                
                // First access the object key
                match current {
                    Value::Object(obj) => {
                        if let Some(next) = obj.get(key) {
                            current = next;
                        } else {
                            return Err(anyhow::anyhow!("Path '{}' not found in object", key));
                        }
                    },
                    _ => return Err(anyhow::anyhow!("Cannot access '{}' on non-object", key)),
                }
                
                // Then access the array index
                if let Ok(index) = index_str.parse::<usize>() {
                    match current {
                        Value::Array(arr) => {
                            if let Some(next) = arr.get(index) {
                                current = next;
                            } else {
                                return Err(anyhow::anyhow!("Index {} out of bounds in array", index));
                            }
                        },
                        _ => return Err(anyhow::anyhow!("Cannot access array index on non-array")),
                    }
                } else {
                    return Err(anyhow::anyhow!("Invalid array index: {}", index_str));
                }
            } else {
                // Regular object key access
                match current {
                    Value::Object(obj) => {
                        if let Some(next) = obj.get(part) {
                            current = next;
                        } else {
                            return Err(anyhow::anyhow!("Path '{}' not found in object", part));
                        }
                    },
                    Value::Array(arr) => {
                        if let Ok(index) = part.parse::<usize>() {
                            if let Some(next) = arr.get(index) {
                                current = next;
                            } else {
                                return Err(anyhow::anyhow!("Index {} out of bounds in array", index));
                            }
                        } else {
                            return Err(anyhow::anyhow!("Invalid array index: {}", part));
                        }
                    },
                    _ => return Err(anyhow::anyhow!("Cannot access '{}' on non-object/non-array", part)),
                }
            }
        }

        Ok(current.clone())
    }

    fn convert_to_json_schema(&self, type_definition: &Value) -> Result<Value> {
        match type_definition {
            Value::Object(obj) => {
                // Check if this is a field definition (has type, description, required)
                if obj.contains_key("type") {
                    // This is a field definition, convert it
                    let mut property = serde_json::Map::new();
                    
                    // Add type
                    if let Some(field_type) = obj.get("type") {
                        property.insert("type".to_string(), field_type.clone());
                    }
                    
                    // Add description
                    if let Some(description) = obj.get("description") {
                        property.insert("description".to_string(), description.clone());
                    }
                    
                    // Handle nested objects recursively
                    if let Some(properties) = obj.get("properties") {
                        if let Ok(nested_schema) = self.convert_to_json_schema(properties) {
                            property.insert("properties".to_string(), nested_schema);
                        }
                    }
                    
                    // Handle arrays
                    if let Some(items) = obj.get("items") {
                        if let Ok(item_schema) = self.convert_to_json_schema(items) {
                            property.insert("items".to_string(), item_schema);
                        }
                    }
                    
                    Ok(Value::Object(property))
                } else {
                    // This is a type definition with multiple fields
                    let mut schema = serde_json::Map::new();
                    schema.insert("type".to_string(), Value::String("object".to_string()));
                    
                    // Add strict validation - no additional properties allowed
                    schema.insert("additionalProperties".to_string(), Value::Bool(false));
                    
                    // Use Vec to preserve field order
                    let mut properties_vec = Vec::new();
                    let mut required = Vec::new();
                    
                    for (field_name, field_def) in obj {
                        if let Ok(converted_field) = self.convert_to_json_schema(field_def) {
                            properties_vec.push((field_name.clone(), converted_field));
                            
                            // Check if this field is required
                            if let Some(field_obj) = field_def.as_object() {
                                if let Some(required_val) = field_obj.get("required") {
                                    if required_val.as_bool().unwrap_or(false) {
                                        required.push(field_name.clone());
                                    }
                                }
                            }
                        }
                    }
                    
                    // Convert Vec to Map while preserving order
                    let mut properties = serde_json::Map::new();
                    for (key, value) in properties_vec {
                        properties.insert(key, value);
                    }
                    
                    schema.insert("properties".to_string(), Value::Object(properties));
                    if !required.is_empty() {
                        schema.insert("required".to_string(), Value::Array(required.into_iter().map(Value::String).collect()));
                    }
                    
                    Ok(Value::Object(schema))
                }
            }
            Value::Array(arr) => {
                // Handle arrays
                let mut schema = serde_json::Map::new();
                schema.insert("type".to_string(), Value::String("array".to_string()));
                
                if let Some(first_item) = arr.first() {
                    if let Ok(item_schema) = self.convert_to_json_schema(first_item) {
                        schema.insert("items".to_string(), item_schema);
                    }
                }
                
                Ok(Value::Object(schema))
            }
            Value::String(s) => {
                // Handle primitive types
                let mut schema = serde_json::Map::new();
                
                // For "object" type, be more flexible to allow arrays and objects
                if s == "object" {
                    // Don't validate the structure, just accept any JSON value
                    // This allows arrays, objects, or any other JSON structure
                    return Ok(Value::Object(schema));
                } else {
                    schema.insert("type".to_string(), Value::String(s.clone()));
                }
                
                Ok(Value::Object(schema))
            }
            _ => Err(anyhow::anyhow!("Unsupported type definition format"))
        }
    }

    async fn build_action_tree(&self,
        action_ref: &str,
        // The parent id is null initially, but during recursion we pass it down to the children
        parent_action_id: Option<&str>) -> Result<ShAction> {
        // 1. Download the manifest for the current action
        let manifest = self.fetch_manifest(action_ref).await?;
        
        // 2. Create action state
        // Create a unique ID for the action
        let action_id = uuid::Uuid::new_v4().to_string();
        let action_id_for_children = action_id.clone();

        // 3. Build action state
        let mut action_state = ShAction {
            id: action_id,
            name: manifest.name.clone(),
            kind: match &manifest.kind {
                Some(ShKind::Composition) => "composition".to_string(),
                Some(ShKind::Wasm) => "wasm".to_string(),
                Some(ShKind::Docker) => "docker".to_string(),
                None => return Err(anyhow::anyhow!("Unknown manifest kind for action: {}", action_ref))
            },
            uses: action_ref.to_string(),
            // Initially empty inputs and outputs
            inputs: manifest.inputs.as_array()
            .map(|arr| {
                arr.iter().filter_map(|input| {
                    if let Some(obj) = input.as_object() {
                        Some(ShIO {
                            name: obj.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            r#type: obj.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            template: obj.get("value").cloned().unwrap_or(serde_json::Value::Null),
                            value: None,
                            required: obj.get("required").and_then(|v| v.as_bool()).unwrap_or(false),
                        })
                    } else {
                        None
                    }
                }).collect()
            })
            .unwrap_or_default(),
            outputs: manifest.outputs.as_array()
                .map(|arr| {
                    arr.iter().filter_map(|output| {
                        if let Some(obj) = output.as_object() {
                            Some(ShIO {
                                name: obj.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                r#type: obj.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                template: obj.get("value").cloned().unwrap_or(serde_json::Value::Null),
                                value: None,
                                required: obj.get("required").and_then(|v| v.as_bool()).unwrap_or(false),
                            })
                        } else {
                            None
                        }
                    }).collect()
                })
                .unwrap_or_default(),
            parent_action: parent_action_id.map(|s| s.to_string()),
            steps: HashMap::new(),
            role: manifest.role,
            // Initially empty types
            types: if manifest.types.is_empty() { None } else { Some(manifest.types.clone().into_iter().collect()) },
            // Mirrors from manifest
            mirrors: manifest.mirrors.clone(),
            // Permissions from manifest
            permissions: manifest.permissions.clone(),
        };
        
                // 4. For each step, call the build_action_tree function recursively
        for (_step_name, step_value) in manifest.steps {
            if let Some(uses_value) = step_value.get("uses") {
                if let Some(uses_str) = uses_value.as_str() {
                    let mut child_action = Box::pin(self.build_action_tree(
                        uses_str,
                        Some(&action_id_for_children)
                    )).await?;
                    
                     // Extract step inputs and inject them into the child action
                    if let Some(step_inputs) = step_value.get("inputs") {
                        if let Some(inputs_array) = step_inputs.as_array() {
                            for (index, input) in inputs_array.iter().enumerate() {
                                if let Some(input_obj) = input.as_object() {
                                    if let Some(child_input) = child_action.inputs.get_mut(index) {
                                        // Inject the template value from the step input
                                        if let Some(template_value) = input_obj.get("value") {
                                            child_input.template = template_value.clone();
                                        }
                                    }
                                }
                            }
                        }
                    }

                    
                    // Add child to parent's children HashMap
                    action_state.steps.insert(_step_name.clone(), child_action);
                }
            }
        }

        return Ok(action_state);
    }
    
    pub fn find_sibling_dependencies(&self, value: &Value, steps: &HashMap<String, ShAction>) -> Result<Vec<String>> {        
                // Look for patterns like {{steps.step_name.field}}
                let re = regex::Regex::new(r"\{\{steps\.([^.]+)")?;
        let mut deps = std::collections::HashSet::new();
                
        match value {
            // The template could be a string, an object or an array
            Value::String(s) => {      
                for cap in re.captures_iter(s) {
                    if let Some(step_name) = cap.get(1) {
                        let step_name = step_name.as_str();
                        // Find the step ID that corresponds to this step name
                        let step = steps.get(step_name);
                        if let Some(_step) = step {
                            deps.insert(step_name.to_string());
                        }
                    }
                }
                Ok(deps.into_iter().collect())
            },
            // If the value is an object, we need to find the dependencies int the object
            // recursively
            Value::Object(obj) => {
                for (_, v) in obj {
                    let child_deps = self.find_sibling_dependencies(v, steps)?;
                    deps.extend(child_deps);
                }
                Ok(deps.into_iter().collect())
            },
            // If the value is an array, we need to find the dependencies in the array
            // recursively
            Value::Array(arr) => {
                for item in arr {
                    let child_deps = self.find_sibling_dependencies(item, steps)?;
                    deps.extend(child_deps);
                }
                Ok(deps.into_iter().collect())
            },
            _ => Ok(Vec::new())
        }
    }

    
    /// Checks if a value contains unresolved template syntax
    fn contains_unresolved_templates(&self, value: &Value) -> bool {
        match value {
            Value::String(s) => s.contains("{{") || s.contains("}}"),
            Value::Object(obj) => {
                for (_, v) in obj {
                    if self.contains_unresolved_templates(v) {
                        return true;
                    }
                }
                false
            },
            Value::Array(arr) => {
                for item in arr {
                    if self.contains_unresolved_templates(item) {
                        return true;
                    }
                }
                false
            },
            _ => false, // Numbers, booleans, null don't contain templates
        }
    }




    /// Finds all ready steps - steps where all inputs are resolved and at least one output is not populated
    fn find_ready_step_ids(
        &self,
        steps: &HashMap<String, ShAction>,
    ) -> Result<Vec<String>> {
        let mut ready_steps = Vec::new();
        
        for (step_id, step) in steps {
            // Check if all inputs have been resolved (every input has a "value" field populated)
            let all_inputs_resolved = step.inputs.iter().all(|input| {
                input.value.is_some()
            });

            // Check if at least one output has not been populated yet
            let has_unresolved_outputs = step.outputs.iter().any(|output| {
                output.value.is_none()
            });

            if all_inputs_resolved && has_unresolved_outputs {
                ready_steps.push(step_id.clone());
            }
        }
        
        Ok(ready_steps)
    }

    /// Finds steps that depend on a completed step and are now ready to execute
    fn find_downstream_ready_steps_keys(
        &self,
        steps: &HashMap<String, ShAction>,
        previous_step_id: &str,
        parent_inputs: &Vec<ShIO>,
    ) -> Result<Vec<String>> {
        let mut dependent_ready_steps: Vec<String> = Vec::new();
        
        // if the current step is a flow control step, we want to find it among the steps, 
        // get the next step by using the first output of the step we have just executed.
        if let Some(step) = steps.get(previous_step_id) {
            if step.role.as_ref().map_or(false, |r| r == &ShRole::FlowControl) {
                if let Some(output) = step.outputs.first() {
                    if let Some(output_value) = &output.value {
                        if let Some(output_value_str) = output_value.as_str() {
                            let next_step_id = output_value_str;
                            // Check if the step exists in the steps vector
                            if steps.contains_key(next_step_id) {
                                println!("Adding because of flow control step from previous step: {:?} -> {:?}", previous_step_id, next_step_id);
                                dependent_ready_steps.push(next_step_id.to_string());
                            }
                        }
                    }
                }
            }
        }
            
        for (step_id, step) in steps {
            // Skip if this is the step we just completed
            if step_id == previous_step_id {
                continue;
            }

            let depends_on = self.step_depends_on(step, previous_step_id);
            let is_ready = self.are_all_inputs_ready(step, parent_inputs)?;

            // Check if this step depends on the completed step and is now ready
            if depends_on && is_ready {
                dependent_ready_steps.push(step_id.clone());
            }
        }
        
        Ok(dependent_ready_steps)
    }

    /// Checks if all step inputs are ready (have values)
    fn are_all_inputs_ready(
        &self,
        step: &ShAction,
        _parent_inputs: &Vec<ShIO>,
    ) -> Result<bool> {
        // Check if all input values are populated (not null)
        let all_inputs_resolved = step.inputs.iter().all(|input| {
            input.value.is_some()
        });
        
        Ok(all_inputs_resolved)
    }

    /// Checks if a step depends on another step (simplified dependency check)
    fn step_depends_on(&self, step: &ShAction, dependency_step_id: &str) -> bool {
        // Check if any of the step's input templates reference the dependency step
        for input in &step.inputs {
            if self.value_contains_dependency(&input.template, dependency_step_id) {
                    return true;
                }
        }
        false
    }

    /// Recursively checks if a Value contains a dependency reference
    fn value_contains_dependency(&self, value: &Value, dependency_step_id: &str) -> bool {
        match value {
            Value::String(template) => {
                template.contains(&format!("steps.{}", dependency_step_id))
            }
            Value::Object(map) => {
                // Recursively check all values in the object
                for (_, v) in map {
                    if self.value_contains_dependency(v, dependency_step_id) {
                        return true;
            }
        }
        false
            }
            Value::Array(arr) => {
                // Recursively check all values in the array
                for v in arr {
                    if self.value_contains_dependency(v, dependency_step_id) {
                        return true;
                    }
                }
                false
            }
            _ => false, // Numbers, booleans, null don't contain dependencies
        }
    }

    // Fetches the manifest and parses into an ShManifest object
    async fn fetch_manifest(&self, action_ref: &str) -> Result<ShManifest> {
        // Construct storage URL for starthub-lock.json
        let url_path = action_ref.replace(":", "/");
        let storage_url = format!(
            "{}{}/{}/{}",
            STARTHUB_API_BASE_URL,
            STARTHUB_STORAGE_PATH,
            url_path,
            STARTHUB_MANIFEST_FILENAME
        );

        // Download and parse starthub-lock.json
        let client = reqwest::Client::new();
        let response = client.get(&storage_url).send().await?;
        
        if response.status().is_success() {
            // Log the response body for debugging
            let response_text = response.text().await?;            
            // Try to parse the JSON
            let manifest: ShManifest = serde_json::from_str(&response_text)
                .map_err(|e| anyhow::anyhow!("JSON parsing error: {} - Response: {}", e, response_text))?;
        Ok(manifest)
        } else {
            Err(anyhow::anyhow!("Failed to download starthub-lock.json: {}", response.status()))
        }
    }

    fn resolve_parent_inputs_into_steps(&self, action_inputs: &Vec<ShIO>, action_steps: &HashMap<String, ShAction>) -> HashMap<String, ShAction> {
        action_steps.iter()
            .map(|(step_id, step)| {
                if let Some(resolved_inputs_to_inject_into_child_step) = self.resolve_from_parent_inputs(
                    &step.inputs,
                    action_inputs
                ) {

                    let inputs_to_inject = self.instantiate_io(
                        &step.inputs, 
                        &resolved_inputs_to_inject_into_child_step,
                        &step.types,
                        step.role.as_ref()).ok();
                    
                    if let Some(inputs_to_inject) = inputs_to_inject {
                        // Create new step with injected inputs
                        let new_step = ShAction {
                            inputs: inputs_to_inject,
                            ..step.clone()
                        };
                        (step_id.clone(), new_step)
                    } else {
                        // Keep original step if injection failed
                        (step_id.clone(), step.clone())
                    }
                } else {
                    // Keep original step if resolution failed
                    (step_id.clone(), step.clone())
                }
            })
            .collect()
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    
    use serde_json::json;

    #[test]
    fn test_step_depends_on() {
        // Create a mock ExecutionEngine
        let engine = ExecutionEngine::new();
        
        // Test case 1: Step depends on another step (positive case)
        let step_with_dependency = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "string".to_string(),
                    template: Value::String("steps.step2.output".to_string()),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(engine.step_depends_on(&step_with_dependency, "step2"));
        assert!(!engine.step_depends_on(&step_with_dependency, "step3"));
        
        // Test case 2: Step does not depend on any step (negative case)
        let step_without_dependency = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "string".to_string(),
                    template: Value::String("static_value".to_string()),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(!engine.step_depends_on(&step_without_dependency, "step2"));
        
        // Test case 3: Step with multiple inputs, one depends on another step
        let step_with_multiple_inputs = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "string".to_string(),
                    template: Value::String("static_value".to_string()),
                    value: None,
                    required: true,
                },
                ShIO {
                    name: "input2".to_string(),
                    r#type: "string".to_string(),
                    template: Value::String("steps.step3.result".to_string()),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(engine.step_depends_on(&step_with_multiple_inputs, "step3"));
        assert!(!engine.step_depends_on(&step_with_multiple_inputs, "step2"));
        
        // Test case 4: Step with non-string template (should not match)
        let step_with_non_string_template = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "number".to_string(),
                    template: Value::Number(serde_json::Number::from(42)),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(!engine.step_depends_on(&step_with_non_string_template, "step2"));
        
        // Test case 5: Step with empty inputs
        let step_with_empty_inputs = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(!engine.step_depends_on(&step_with_empty_inputs, "step2"));
        
        // Test case 6: Step with partial match in template (should match because contains() is used)
        let step_with_partial_match = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "string".to_string(),
                    template: Value::String("some_steps.step2.other".to_string()),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(engine.step_depends_on(&step_with_partial_match, "step2"));
        
        // Test case 7: Step with exact match format
        let step_with_exact_match = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "string".to_string(),
                    template: Value::String("steps.step2".to_string()),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(engine.step_depends_on(&step_with_exact_match, "step2"));
        
        // Test case 8: Step with no dependency (true negative case)
        let step_with_no_dependency = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "string".to_string(),
                    template: Value::String("completely_different_string".to_string()),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(!engine.step_depends_on(&step_with_no_dependency, "step2"));
        
        // Test case 9: Step with object template containing dependency (should not match - only string templates are checked)
        let step_with_object_template = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "object".to_string(),
                    template: Value::Object({
                        let mut map = serde_json::Map::new();
                        map.insert("url".to_string(), Value::String("https://api.example.com/data?q={{steps.step2.result}}".to_string()));
                        map.insert("headers".to_string(), Value::Object({
                            let mut headers_map = serde_json::Map::new();
                            headers_map.insert("Content-Type".to_string(), Value::String("application/json".to_string()));
                            headers_map.insert("Authorization".to_string(), Value::String("Bearer {{steps.step2.token}}".to_string()));
                            headers_map
                        }));
                        map
                    }),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        // Now that step_depends_on recursively searches objects, this should be true
        assert!(engine.step_depends_on(&step_with_object_template, "step2"));
        
        // Test case 10: Step with nested object template containing dependency (now matches with recursive search)
        let step_with_nested_object_template = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "object".to_string(),
                    template: Value::Object({
                        let mut map = serde_json::Map::new();
                        map.insert("local_names".to_string(), Value::Object({
                            let mut inner_map = serde_json::Map::new();
                            inner_map.insert("en".to_string(), Value::String("{{steps.step3.outputs[0].body[0].local_names.en}}".to_string()));
                            inner_map.insert("it".to_string(), Value::String("{{steps.step3.outputs[0].body[0].local_names.it}}".to_string()));
                            inner_map
                        }));
                        map.insert("lat".to_string(), Value::String("{{steps.step3.outputs[0].body[0].lat}}".to_string()));
                        map.insert("lon".to_string(), Value::String("{{steps.step3.outputs[0].body[0].lon}}".to_string()));
                        map
                    }),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(engine.step_depends_on(&step_with_nested_object_template, "step3"));
        assert!(!engine.step_depends_on(&step_with_nested_object_template, "step2"));
        
        // Test case 11: Step with array template containing dependency (now matches with recursive search)
        let step_with_array_template = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "array".to_string(),
                    template: Value::Array(vec![
                        Value::String("{{steps.step4.outputs[0].body[0].lat}}".to_string()),
                        Value::String("{{steps.step4.outputs[0].body[0].lon}}".to_string()),
                        Value::String("{{steps.step4.outputs[0].body[0].country}}".to_string()),
                        Value::String("static_value".to_string()),
                        Value::Number(serde_json::Number::from(42))
                    ]),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(engine.step_depends_on(&step_with_array_template, "step4"));
        assert!(!engine.step_depends_on(&step_with_array_template, "step2"));
        
        // Test case 12: Step with complex nested structure containing dependency (now matches with recursive search)
        let step_with_complex_template = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "object".to_string(),
                    template: Value::Object({
                        let mut map = serde_json::Map::new();
                        map.insert("local_names".to_string(), Value::Object({
                            let mut local_names_map = serde_json::Map::new();
                            local_names_map.insert("en".to_string(), Value::String("{{steps.step5.outputs[0].body[0].local_names.en}}".to_string()));
                            local_names_map.insert("it".to_string(), Value::String("{{steps.step5.outputs[0].body[0].local_names.it}}".to_string()));
                            local_names_map.insert("fr".to_string(), Value::String("{{steps.step5.outputs[0].body[0].local_names.fr}}".to_string()));
                            local_names_map
                        }));
                        map.insert("lat".to_string(), Value::String("{{steps.step6.outputs[0].body[0].lat}}".to_string()));
                        map.insert("lon".to_string(), Value::String("{{steps.step6.outputs[0].body[0].lon}}".to_string()));
                        map.insert("country".to_string(), Value::String("{{steps.step6.outputs[0].body[0].country}}".to_string()));
                        map.insert("state".to_string(), Value::String("{{steps.step6.outputs[0].body[0].state}}".to_string()));
                        map
                    }),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(engine.step_depends_on(&step_with_complex_template, "step5"));
        assert!(engine.step_depends_on(&step_with_complex_template, "step6"));
        assert!(!engine.step_depends_on(&step_with_complex_template, "step2"));
        
        // Test case 13: Step with object template but no dependency
        let step_with_object_no_dependency = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "object".to_string(),
                    template: Value::Object({
                        let mut map = serde_json::Map::new();
                        map.insert("lat".to_string(), Value::String("40.7128".to_string()));
                        map.insert("lon".to_string(), Value::String("-74.0060".to_string()));
                        map.insert("country".to_string(), Value::String("US".to_string()));
                        map.insert("state".to_string(), Value::String("NY".to_string()));
                        map
                    }),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(!engine.step_depends_on(&step_with_object_no_dependency, "step2"));
        
        // Test case 14: Step with string template containing JSON-like object (should match)
        let step_with_json_string_template = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "string".to_string(),
                    template: Value::String(r#"{"source": "steps.step7.data", "type": "json"}"#.to_string()),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(engine.step_depends_on(&step_with_json_string_template, "step7"));
        assert!(!engine.step_depends_on(&step_with_json_string_template, "step2"));
        
        // Test case 15: Step with string template containing multiple dependencies
        let step_with_multiple_dependencies = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "string".to_string(),
                    template: Value::String("steps.step8.output and steps.step9.result".to_string()),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(engine.step_depends_on(&step_with_multiple_dependencies, "step8"));
        assert!(engine.step_depends_on(&step_with_multiple_dependencies, "step9"));
        assert!(!engine.step_depends_on(&step_with_multiple_dependencies, "step2"));
        
        // Test case 16: Mixed inputs - one string with dependency, one object without dependency
        let step_with_mixed_inputs = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "string".to_string(),
                    template: Value::String("steps.step10.output".to_string()),
                    value: None,
                    required: true,
                },
                ShIO {
                    name: "input2".to_string(),
                    r#type: "object".to_string(),
                    template: Value::Object({
                        let mut map = serde_json::Map::new();
                        map.insert("lat".to_string(), Value::String("{{steps.step11.outputs[0].body[0].lat}}".to_string()));
                        map.insert("lon".to_string(), Value::String("{{steps.step11.outputs[0].body[0].lon}}".to_string()));
                        map.insert("country".to_string(), Value::String("{{steps.step11.outputs[0].body[0].country}}".to_string()));
                        map
                    }),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(engine.step_depends_on(&step_with_mixed_inputs, "step10"));
        assert!(engine.step_depends_on(&step_with_mixed_inputs, "step11")); // Object template now checked recursively
        assert!(!engine.step_depends_on(&step_with_mixed_inputs, "step2"));
        
        // Test case 17: Step with string template using correct {{}} format (like in starthub-lock.json)
        let step_with_correct_template_format = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "string".to_string(),
                    template: Value::String("https://api.example.com/data?q={{steps.step12.result}}&key={{steps.step13.api_key}}".to_string()),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(engine.step_depends_on(&step_with_correct_template_format, "step12"));
        assert!(engine.step_depends_on(&step_with_correct_template_format, "step13"));
        assert!(!engine.step_depends_on(&step_with_correct_template_format, "step2"));
        
        // Test case 18: Step with object template using correct {{}} format (now matches with recursive search)
        let step_with_object_correct_format = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "object".to_string(),
                    template: Value::Object({
                        let mut map = serde_json::Map::new();
                        map.insert("url".to_string(), Value::String("https://api.openweathermap.org/geo/1.0/direct?q={{steps.step14.outputs[0].body[0].location_name}}&limit=1&appid={{steps.step15.outputs[0].body[0].open_weather_api_key}}".to_string()));
                        map.insert("headers".to_string(), Value::Object({
                            let mut headers_map = serde_json::Map::new();
                            headers_map.insert("Content-Type".to_string(), Value::String("application/json".to_string()));
                            headers_map.insert("Authorization".to_string(), Value::String("Bearer {{steps.step16.outputs[0].body[0].token}}".to_string()));
                            headers_map
                        }));
                        map
                    }),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(engine.step_depends_on(&step_with_object_correct_format, "step14"));
        assert!(engine.step_depends_on(&step_with_object_correct_format, "step15"));
        assert!(engine.step_depends_on(&step_with_object_correct_format, "step16"));
        
        // Test case 19: Step with number template (should not match)
        let step_with_number_template = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "number".to_string(),
                    template: Value::Number(serde_json::Number::from(42)),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(!engine.step_depends_on(&step_with_number_template, "step2"));
        
        // Test case 20: Step with boolean template (should not match)
        let step_with_boolean_template = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "boolean".to_string(),
                    template: Value::Bool(true),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(!engine.step_depends_on(&step_with_boolean_template, "step2"));
        
        // Test case 21: Step with null template (should not match)
        let step_with_null_template = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "null".to_string(),
                    template: Value::Null,
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(!engine.step_depends_on(&step_with_null_template, "step2"));
        
        // Test case 22: Step with mixed types including numbers and booleans (should only match strings)
        let step_with_mixed_types = ShAction {
            id: "step1".to_string(),
            name: "test_step".to_string(),
            kind: "wasm".to_string(),
            uses: "test/action:1.0.0".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "object".to_string(),
                    template: Value::Object({
                        let mut map = serde_json::Map::new();
                        map.insert("string_field".to_string(), Value::String("{{steps.step17.result}}".to_string()));
                        map.insert("number_field".to_string(), Value::Number(serde_json::Number::from(42)));
                        map.insert("boolean_field".to_string(), Value::Bool(true));
                        map.insert("null_field".to_string(), Value::Null);
                        map.insert("array_field".to_string(), Value::Array(vec![
                            Value::String("{{steps.step18.data}}".to_string()),
                            Value::Number(serde_json::Number::from(100)),
                            Value::Bool(false)
                        ]));
                        map
                    }),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            role: None,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        assert!(engine.step_depends_on(&step_with_mixed_types, "step17"));
        assert!(engine.step_depends_on(&step_with_mixed_types, "step18"));
        assert!(!engine.step_depends_on(&step_with_mixed_types, "step2"));
    }

    #[test]
    fn test_contains_unresolved_templates() {
        // Create a mock ExecutionEngine
        let engine = ExecutionEngine::new();
        
        // Test case 1: String with unresolved template (should return true)
        let string_with_template = Value::String("Hello {{steps.step1.result}} world".to_string());
        assert!(engine.contains_unresolved_templates(&string_with_template));
        
        // Test case 2: String with only opening braces (should return true)
        let string_with_opening_braces = Value::String("Hello {{steps.step1.result world".to_string());
        assert!(engine.contains_unresolved_templates(&string_with_opening_braces));
        
        // Test case 3: String with only closing braces (should return true)
        let string_with_closing_braces = Value::String("Hello steps.step1.result}} world".to_string());
        assert!(engine.contains_unresolved_templates(&string_with_closing_braces));
        
        // Test case 4: String without any braces (should return false)
        let string_without_template = Value::String("Hello world".to_string());
        assert!(!engine.contains_unresolved_templates(&string_without_template));
        
        // Test case 5: String with resolved template (should return false)
        let string_with_resolved = Value::String("Hello resolved_value world".to_string());
        assert!(!engine.contains_unresolved_templates(&string_with_resolved));
        
        // Test case 6: Object with unresolved template in string value (should return true)
        let object_with_template = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::String("test".to_string()));
            map.insert("value".to_string(), Value::String("{{steps.step2.data}}".to_string()));
            map.insert("type".to_string(), Value::String("string".to_string()));
            map
        });
        assert!(engine.contains_unresolved_templates(&object_with_template));
        
        // Test case 7: Object without unresolved templates (should return false)
        let object_without_template = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::String("test".to_string()));
            map.insert("value".to_string(), Value::String("resolved_data".to_string()));
            map.insert("type".to_string(), Value::String("string".to_string()));
            map
        });
        assert!(!engine.contains_unresolved_templates(&object_without_template));
        
        // Test case 8: Nested object with unresolved template (should return true)
        let nested_object_with_template = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("config".to_string(), Value::Object({
                let mut inner_map = serde_json::Map::new();
                inner_map.insert("url".to_string(), Value::String("https://api.example.com".to_string()));
                inner_map.insert("token".to_string(), Value::String("{{steps.step3.token}}".to_string()));
                inner_map
            }));
            map.insert("enabled".to_string(), Value::Bool(true));
            map
        });
        assert!(engine.contains_unresolved_templates(&nested_object_with_template));
        
        // Test case 9: Nested object without unresolved templates (should return false)
        let nested_object_without_template = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("config".to_string(), Value::Object({
                let mut inner_map = serde_json::Map::new();
                inner_map.insert("url".to_string(), Value::String("https://api.example.com".to_string()));
                inner_map.insert("token".to_string(), Value::String("resolved_token".to_string()));
                inner_map
            }));
            map.insert("enabled".to_string(), Value::Bool(true));
            map
        });
        assert!(!engine.contains_unresolved_templates(&nested_object_without_template));
        
        // Test case 10: Array with unresolved template (should return true)
        let array_with_template = Value::Array(vec![
            Value::String("item1".to_string()),
            Value::String("{{steps.step4.data}}".to_string()),
            Value::String("item3".to_string())
        ]);
        assert!(engine.contains_unresolved_templates(&array_with_template));
        
        // Test case 11: Array without unresolved templates (should return false)
        let array_without_template = Value::Array(vec![
            Value::String("item1".to_string()),
            Value::String("resolved_data".to_string()),
            Value::String("item3".to_string())
        ]);
        assert!(!engine.contains_unresolved_templates(&array_without_template));
        
        // Test case 12: Nested array with unresolved template (should return true)
        let nested_array_with_template = Value::Array(vec![
            Value::String("item1".to_string()),
            Value::Array(vec![
                Value::String("nested_item1".to_string()),
                Value::String("{{steps.step5.nested_data}}".to_string())
            ]),
            Value::String("item3".to_string())
        ]);
        assert!(engine.contains_unresolved_templates(&nested_array_with_template));
        
        // Test case 13: Complex nested structure with unresolved template (should return true)
        let complex_structure_with_template = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("pipeline".to_string(), Value::Array(vec![
                Value::Object({
                    let mut step_map = serde_json::Map::new();
                    step_map.insert("name".to_string(), Value::String("fetch_data".to_string()));
                    step_map.insert("input".to_string(), Value::String("{{steps.step6.result}}".to_string()));
                    step_map
                }),
                Value::Object({
                    let mut step_map = serde_json::Map::new();
                    step_map.insert("name".to_string(), Value::String("process_data".to_string()));
                    step_map.insert("input".to_string(), Value::String("static_input".to_string()));
                    step_map
                })
            ]));
            map.insert("settings".to_string(), Value::Object({
                let mut settings_map = serde_json::Map::new();
                settings_map.insert("timeout".to_string(), Value::Number(serde_json::Number::from(30)));
                settings_map.insert("retry_source".to_string(), Value::String("{{steps.step7.fallback}}".to_string()));
                settings_map
            }));
            map
        });
        assert!(engine.contains_unresolved_templates(&complex_structure_with_template));
        
        // Test case 14: Complex nested structure without unresolved templates (should return false)
        let complex_structure_without_template = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("pipeline".to_string(), Value::Array(vec![
                Value::Object({
                    let mut step_map = serde_json::Map::new();
                    step_map.insert("name".to_string(), Value::String("fetch_data".to_string()));
                    step_map.insert("input".to_string(), Value::String("resolved_result".to_string()));
                    step_map
                }),
                Value::Object({
                    let mut step_map = serde_json::Map::new();
                    step_map.insert("name".to_string(), Value::String("process_data".to_string()));
                    step_map.insert("input".to_string(), Value::String("static_input".to_string()));
                    step_map
                })
            ]));
            map.insert("settings".to_string(), Value::Object({
                let mut settings_map = serde_json::Map::new();
                settings_map.insert("timeout".to_string(), Value::Number(serde_json::Number::from(30)));
                settings_map.insert("retry_source".to_string(), Value::String("resolved_fallback".to_string()));
                settings_map
            }));
            map
        });
        assert!(!engine.contains_unresolved_templates(&complex_structure_without_template));
        
        // Test case 15: Number value (should return false)
        let number_value = Value::Number(serde_json::Number::from(42));
        assert!(!engine.contains_unresolved_templates(&number_value));
        
        // Test case 16: Boolean value (should return false)
        let boolean_value = Value::Bool(true);
        assert!(!engine.contains_unresolved_templates(&boolean_value));
        
        // Test case 17: Null value (should return false)
        let null_value = Value::Null;
        assert!(!engine.contains_unresolved_templates(&null_value));
        
        // Test case 18: Mixed types with unresolved template (should return true)
        let mixed_types_with_template = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("string_field".to_string(), Value::String("{{steps.step8.result}}".to_string()));
            map.insert("number_field".to_string(), Value::Number(serde_json::Number::from(42)));
            map.insert("boolean_field".to_string(), Value::Bool(true));
            map.insert("null_field".to_string(), Value::Null);
            map.insert("array_field".to_string(), Value::Array(vec![
                Value::String("{{steps.step9.data}}".to_string()),
                Value::Number(serde_json::Number::from(100)),
                Value::Bool(false)
            ]));
            map
        });
        assert!(engine.contains_unresolved_templates(&mixed_types_with_template));
        
        // Test case 19: Mixed types without unresolved templates (should return false)
        let mixed_types_without_template = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("string_field".to_string(), Value::String("resolved_result".to_string()));
            map.insert("number_field".to_string(), Value::Number(serde_json::Number::from(42)));
            map.insert("boolean_field".to_string(), Value::Bool(true));
            map.insert("null_field".to_string(), Value::Null);
            map.insert("array_field".to_string(), Value::Array(vec![
                Value::String("resolved_data".to_string()),
                Value::Number(serde_json::Number::from(100)),
                Value::Bool(false)
            ]));
            map
        });
        assert!(!engine.contains_unresolved_templates(&mixed_types_without_template));
        
        // Test case 20: Edge case - empty string (should return false)
        let empty_string = Value::String("".to_string());
        assert!(!engine.contains_unresolved_templates(&empty_string));
        
        // Test case 21: Edge case - string with only braces (should return true)
        let string_with_only_braces = Value::String("{{}}".to_string());
        assert!(engine.contains_unresolved_templates(&string_with_only_braces));
        
        // Test case 22: Edge case - string with malformed template (should return true)
        let string_with_malformed_template = Value::String("{{steps.step10.result".to_string());
        assert!(engine.contains_unresolved_templates(&string_with_malformed_template));
        
        // Test case 23: Edge case - string with multiple template patterns (should return true)
        let string_with_multiple_templates = Value::String("{{steps.step11.result}} and {{steps.step12.data}}".to_string());
        assert!(engine.contains_unresolved_templates(&string_with_multiple_templates));
        
        // Test case 24: Edge case - string with nested braces (should return true)
        let string_with_nested_braces = Value::String("{{steps.{{step13.nested}}.result}}".to_string());
        assert!(engine.contains_unresolved_templates(&string_with_nested_braces));
        
        // Test case 25: Edge case - string with escaped braces (should return false)
        let string_with_escaped_braces = Value::String("\\{\\{steps.step14.result\\}\\}".to_string());
        assert!(!engine.contains_unresolved_templates(&string_with_escaped_braces));
        
        // Test case 26: Edge case - empty object (should return false)
        let empty_object = Value::Object(serde_json::Map::new());
        assert!(!engine.contains_unresolved_templates(&empty_object));
        
        // Test case 27: Edge case - empty array (should return false)
        let empty_array = Value::Array(vec![]);
        assert!(!engine.contains_unresolved_templates(&empty_array));
        
        // Test case 28: Edge case - object with only primitive values (should return false)
        let object_with_only_primitives = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("number".to_string(), Value::Number(serde_json::Number::from(42)));
            map.insert("boolean".to_string(), Value::Bool(true));
            map.insert("null".to_string(), Value::Null);
            map
        });
        assert!(!engine.contains_unresolved_templates(&object_with_only_primitives));
        
        // Test case 29: Edge case - array with only primitive values (should return false)
        let array_with_only_primitives = Value::Array(vec![
            Value::Number(serde_json::Number::from(1)),
            Value::Bool(false),
            Value::Null
        ]);
        assert!(!engine.contains_unresolved_templates(&array_with_only_primitives));
        
        // Test case 30: Edge case - very deep nesting with template (should return true)
        let deep_nesting_with_template = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("level1".to_string(), Value::Object({
                let mut level1_map = serde_json::Map::new();
                level1_map.insert("level2".to_string(), Value::Object({
                    let mut level2_map = serde_json::Map::new();
                    level2_map.insert("level3".to_string(), Value::Array(vec![
                        Value::Object({
                            let mut level3_map = serde_json::Map::new();
                            level3_map.insert("deep_value".to_string(), Value::String("{{steps.step15.deep.result}}".to_string()));
                            level3_map
                        })
                    ]));
                    level2_map
                }));
                level1_map
            }));
            map
        });
        assert!(engine.contains_unresolved_templates(&deep_nesting_with_template));
        
        // Test case 31: Edge case - very deep nesting without template (should return false)
        let deep_nesting_without_template = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("level1".to_string(), Value::Object({
                let mut level1_map = serde_json::Map::new();
                level1_map.insert("level2".to_string(), Value::Object({
                    let mut level2_map = serde_json::Map::new();
                    level2_map.insert("level3".to_string(), Value::Array(vec![
                        Value::Object({
                            let mut level3_map = serde_json::Map::new();
                            level3_map.insert("deep_value".to_string(), Value::String("resolved_deep_value".to_string()));
                            level3_map
                        })
                    ]));
                    level2_map
                }));
                level1_map
            }));
            map
        });
        assert!(!engine.contains_unresolved_templates(&deep_nesting_without_template));
    }

    #[test]
    fn test_interpolate_string() {
        let engine = ExecutionEngine::new();
        
        // Test case 1: Simple input interpolation without jsonpath
        let template1 = "Hello {{inputs[0]}} world!";
        let variables1 = vec![Value::String("John".to_string())];
        let _executed_steps1: HashMap<String, ShAction> = HashMap::new();
        let result1 = engine.interpolate_string_from_parent_input(template1, &variables1).unwrap();
        assert_eq!(result1, "Hello John world!");
        
        // Test case 2: Multiple input interpolations
        let template2 = "{{inputs[0]}} and {{inputs[1]}} are friends";
        let variables2 = vec![
            Value::String("Alice".to_string()),
            Value::String("Bob".to_string())
        ];
        let _executed_steps2: HashMap<String, ShAction> = HashMap::new();
        let result2 = engine.interpolate_string_from_parent_input(template2, &variables2).unwrap();
        assert_eq!(result2, "Alice and Bob are friends");
        
        // Test case 3: Input interpolation with non-string values
        let template3 = "The number is {{inputs[0]}}";
        let variables3 = vec![Value::Number(serde_json::Number::from(42))];
        let _executed_steps3: HashMap<String, ShAction> = HashMap::new();
        let result3 = engine.interpolate_string_from_parent_input(template3, &variables3).unwrap();
        assert_eq!(result3, "The number is 42");
        
        // Test case 4: Input interpolation with boolean values
        let template4 = "Status: {{inputs[0]}}";
        let variables4 = vec![Value::Bool(true)];
        let _executed_steps4: HashMap<String, ShAction> = HashMap::new();
        let result4 = engine.interpolate_string_from_parent_input(template4, &variables4).unwrap();
        assert_eq!(result4, "Status: true");
        
        // Test case 5: Input interpolation with null values
        let template5 = "Value: {{inputs[0]}}";
        let variables5 = vec![Value::Null];
        let _executed_steps5: HashMap<String, ShAction> = HashMap::new();
        let result5 = engine.interpolate_string_from_parent_input(template5, &variables5).unwrap();
        assert_eq!(result5, "Value: null");
        
        // Test case 6: Input interpolation with JSONPath
        let template6 = "Name: {{inputs[0].name}}";
        let variables6 = vec![Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::String("Charlie".to_string()));
            map.insert("age".to_string(), Value::Number(serde_json::Number::from(30)));
            map
        })];
        let _executed_steps6: HashMap<String, ShAction> = HashMap::new();
        let result6 = engine.interpolate_string_from_parent_input(template6, &variables6).unwrap();
        assert_eq!(result6, "Name: Charlie");
        
        // Test case 7: Input interpolation with nested JSONPath
        let template7 = "City: {{inputs[0].address.city}}";
        let variables7 = vec![Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::String("David".to_string()));
            map.insert("address".to_string(), Value::Object({
                let mut addr_map = serde_json::Map::new();
                addr_map.insert("city".to_string(), Value::String("New York".to_string()));
                addr_map.insert("country".to_string(), Value::String("USA".to_string()));
                addr_map
            }));
            map
        })];
        let _executed_steps7: HashMap<String, ShAction> = HashMap::new();
        let result7 = engine.interpolate_string_from_parent_input(template7, &variables7).unwrap();
        assert_eq!(result7, "City: New York");
        
        // Test case 8: Input interpolation with array access
        let template8 = "First item: {{inputs[0].items.0}}";
        let variables8 = vec![Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("items".to_string(), Value::Array(vec![
                Value::String("apple".to_string()),
                Value::String("banana".to_string())
            ]));
            map
        })];
        let _executed_steps8: HashMap<String, ShAction> = HashMap::new();
        let result8 = engine.interpolate_string_from_parent_input(template8, &variables8).unwrap();
        assert_eq!(result8, "First item: apple");
        
        // Test case 9: Step output interpolation without jsonpath
        let template9 = "Result: {{steps.step1.outputs[0]}}";
        let executed_steps9 = {
            let mut map = HashMap::new();
            let step1 = ShAction {
                id: "step1".to_string(),
                name: "test_step".to_string(),
                kind: "wasm".to_string(),
                uses: "test/action:1.0.0".to_string(),
                inputs: vec![],
                outputs: vec![
                    ShIO {
                        name: "result".to_string(),
                        r#type: "string".to_string(),
                        template: Value::String("test_result".to_string()),
                        value: Some(Value::String("Hello from step1".to_string())),
                        required: true,
                    }
                ],
                parent_action: None,
                steps: HashMap::new(),
                role: None,
                types: None,
                mirrors: vec![],
                permissions: None,
            };
            map.insert("step1".to_string(), step1);
            map
        };
        let result9 = engine.interpolate_string_from_sibling_output(template9, &executed_steps9).unwrap();
        assert_eq!(result9, "Result: Hello from step1");
        
        // Test case 10: Step output interpolation with jsonpath
        let template10 = "Name: {{steps.step2.outputs[0].name}}";
        let executed_steps10 = {
            let mut map = HashMap::new();
            let step2 = ShAction {
                id: "step2".to_string(),
                name: "test_step2".to_string(),
                kind: "wasm".to_string(),
                uses: "test/action:1.0.0".to_string(),
                inputs: vec![],
                outputs: vec![
                    ShIO {
                        name: "data".to_string(),
                        r#type: "object".to_string(),
                        template: Value::String("test_data".to_string()),
                        value: Some(Value::Object({
                            let mut data_map = serde_json::Map::new();
                            data_map.insert("name".to_string(), Value::String("Eve".to_string()));
                            data_map.insert("age".to_string(), Value::Number(serde_json::Number::from(25)));
                            data_map
                        })),
                        required: true,
                    }
                ],
                parent_action: None,
                steps: HashMap::new(),
                role: None,
                types: None,
                mirrors: vec![],
                permissions: None,
            };
            map.insert("step2".to_string(), step2);
            map
        };
        let result10 = engine.interpolate_string_from_sibling_output(template10, &executed_steps10).unwrap();
        assert_eq!(result10, "Name: Eve");
        
        // Test case 11: Mixed input and step interpolation
        let template11 = "{{inputs[0]}} used {{steps.step3.outputs[0]}}";
        let variables11 = vec![Value::String("Frank".to_string())];
        let executed_steps11 = {
            let mut map = HashMap::new();
            let step3 = ShAction {
                id: "step3".to_string(),
                name: "test_step3".to_string(),
                kind: "wasm".to_string(),
                uses: "test/action:1.0.0".to_string(),
                inputs: vec![],
                outputs: vec![
                    ShIO {
                        name: "tool".to_string(),
                        r#type: "string".to_string(),
                        template: Value::String("test_tool".to_string()),
                        value: Some(Value::String("hammer".to_string())),
                        required: true,
                    }
                ],
                parent_action: None,
                steps: HashMap::new(),
                role: None,
                types: None,
                mirrors: vec![],
                permissions: None,
            };
            map.insert("step3".to_string(), step3);
            map
        };
        let result11 = engine.interpolate_string_from_parent_input(template11, &variables11).unwrap();
        let result11 = engine.interpolate_string_from_sibling_output(&result11, &executed_steps11).unwrap();
        assert_eq!(result11, "Frank used hammer");
        
        // Test case 12: Template with no interpolation (should return unchanged)
        let template12 = "Hello world!";
        let variables12 = vec![];
        let _executed_steps12: HashMap<String, ShAction> = HashMap::new();
        let result12 = engine.interpolate_string_from_parent_input(template12, &variables12).unwrap();
        assert_eq!(result12, "Hello world!");
        
        // Test case 13: Template with malformed interpolation (should leave unchanged)
        let template13 = "Hello {{inputs[0 world!";
        let variables13 = vec![Value::String("John".to_string())];
        let _executed_steps13: HashMap<String, ShAction> = HashMap::new();
        let result13 = engine.interpolate_string_from_parent_input(template13, &variables13).unwrap();
        assert_eq!(result13, "Hello {{inputs[0 world!");
        
        // Test case 14: Input index out of bounds (should leave unchanged)
        let template14 = "Hello {{inputs[5]}} world!";
        let variables14 = vec![Value::String("John".to_string())];
        let _executed_steps14: HashMap<String, ShAction> = HashMap::new();
        let result14 = engine.interpolate_string_from_parent_input(template14, &variables14).unwrap();
        assert_eq!(result14, "Hello {{inputs[5]}} world!");
        
        // Test case 15: Step not found (should leave unchanged)
        let template15 = "Result: {{steps.nonexistent.outputs[0]}}";
        let executed_steps15 = HashMap::new();
        let result15 = engine.interpolate_string_from_sibling_output(template15, &executed_steps15).unwrap();
        assert_eq!(result15, "Result: {{steps.nonexistent.outputs[0]}}");
        
        // Test case 16: Step output index out of bounds (should leave unchanged)
        let template16 = "Result: {{steps.step1.outputs[5]}}";
        let executed_steps16 = {
            let mut map = HashMap::new();
            let step1 = ShAction {
                id: "step1".to_string(),
                name: "test_step".to_string(),
                kind: "wasm".to_string(),
                uses: "test/action:1.0.0".to_string(),
                inputs: vec![],
                outputs: vec![
                    ShIO {
                        name: "result".to_string(),
                        r#type: "string".to_string(),
                        template: Value::String("test_result".to_string()),
                        value: Some(Value::String("Hello".to_string())),
                        required: true,
                    }
                ],
                parent_action: None,
                steps: HashMap::new(),
                role: None,
                types: None,
                mirrors: vec![],
                permissions: None,
            };
            map.insert("step1".to_string(), step1);
            map
        };
        let result16 = engine.interpolate_string_from_sibling_output(template16, &executed_steps16).unwrap();
        assert_eq!(result16, "Result: {{steps.step1.outputs[5]}}");
        
        // Test case 17: Step output with no value (should leave unchanged)
        let template17 = "Result: {{steps.step4.outputs[0]}}";
        let executed_steps17 = {
            let mut map = HashMap::new();
            let step4 = ShAction {
                id: "step4".to_string(),
                name: "test_step4".to_string(),
                kind: "wasm".to_string(),
                uses: "test/action:1.0.0".to_string(),
                inputs: vec![],
                outputs: vec![
                    ShIO {
                        name: "result".to_string(),
                        r#type: "string".to_string(),
                        template: Value::String("test_result".to_string()),
                        value: None, // No value
                        required: true,
                    }
                ],
                parent_action: None,
                steps: HashMap::new(),
                role: None,
                types: None,
                mirrors: vec![],
                permissions: None,
            };
            map.insert("step4".to_string(), step4);
            map
        };
        let result17 = engine.interpolate_string_from_sibling_output(template17, &executed_steps17).unwrap();
        assert_eq!(result17, "Result: {{steps.step4.outputs[0]}}");
        
        // Test case 18: Complex nested JSONPath
        let template18 = "User: {{inputs[0].user.profile.name}}";
        let variables18 = vec![Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("user".to_string(), Value::Object({
                let mut user_map = serde_json::Map::new();
                user_map.insert("profile".to_string(), Value::Object({
                    let mut profile_map = serde_json::Map::new();
                    profile_map.insert("name".to_string(), Value::String("Grace".to_string()));
                    profile_map.insert("email".to_string(), Value::String("grace@example.com".to_string()));
                    profile_map
                }));
                user_map
            }));
            map
        })];
        let _executed_steps18: HashMap<String, ShAction> = HashMap::new();
        let result18 = engine.interpolate_string_from_parent_input(template18, &variables18).unwrap();
        assert_eq!(result18, "User: Grace");
        
        // Test case 19: Multiple interpolations of same pattern
        let template19 = "{{inputs[0]}} and {{inputs[0]}} are the same";
        let variables19 = vec![Value::String("Henry".to_string())];
        let result19 = engine.interpolate_string_from_parent_input(template19, &variables19).unwrap();
        assert_eq!(result19, "Henry and Henry are the same");
        
        // Test case 20: Empty template
        let template20 = "";
        let variables20 = vec![];
        let result20 = engine.interpolate_string_from_parent_input(template20, &variables20).unwrap();
        assert_eq!(result20, "");
    }

    #[test]
    fn test_interpolate_string_from_parent_input_or_sibling_output() {
        let engine = ExecutionEngine::new();
        
        // Test case 1: Only parent inputs (no sibling outputs)
        let template1 = "Hello {{inputs[0]}} world!";
        let variables1 = vec![Value::String("John".to_string())];
        let _executed_steps1: HashMap<String, ShAction> = HashMap::new();
        let result1 = engine.interpolate_string_from_parent_input_or_sibling_output(template1, &variables1, &_executed_steps1).unwrap();
        assert_eq!(result1, "Hello John world!");
        
        // Test case 2: Only sibling outputs (no parent inputs)
        let template2 = "Result from {{steps.step1.outputs[0]}}";
        let variables2 = vec![];
        let mut executed_steps2: HashMap<String, ShAction> = HashMap::new();
        let step1 = ShAction {
            id: "step1".to_string(),
            name: "step1".to_string(),
            kind: "wasm".to_string(),
            uses: "test-action".to_string(),
            inputs: vec![],
            outputs: vec![ShIO {
                name: "output0".to_string(),
                r#type: "string".to_string(),
                template: Value::String("test_result".to_string()),
                value: Some(Value::String("test_result".to_string())),
                required: false,
            }],
            parent_action: None,
            steps: HashMap::new(),
            types: None,
            role: None,
            mirrors: vec![],
            permissions: None,
        };
        executed_steps2.insert("step1".to_string(), step1);
        let result2 = engine.interpolate_string_from_parent_input_or_sibling_output(template2, &variables2, &executed_steps2).unwrap();
        assert_eq!(result2, "Result from test_result");
        
        // Test case 3: Mixed parent inputs and sibling outputs
        let template3 = "Hello {{inputs[0]}} from {{steps.step1.outputs[0]}}";
        let variables3 = vec![Value::String("John".to_string())];
        let mut executed_steps3: HashMap<String, ShAction> = HashMap::new();
        let step1_3 = ShAction {
            id: "step1".to_string(),
            name: "step1".to_string(),
            kind: "wasm".to_string(),
            uses: "test-action".to_string(),
            inputs: vec![],
            outputs: vec![ShIO {
                name: "output0".to_string(),
                r#type: "string".to_string(),
                template: Value::String("step1_result".to_string()),
                value: Some(Value::String("step1_result".to_string())),
                required: false,
            }],
            parent_action: None,
            steps: HashMap::new(),
            types: None,
            role: None,
            mirrors: vec![],
            permissions: None,
        };
        executed_steps3.insert("step1".to_string(), step1_3);
        let result3 = engine.interpolate_string_from_parent_input_or_sibling_output(template3, &variables3, &executed_steps3).unwrap();
        assert_eq!(result3, "Hello John from step1_result");
        
        // Test case 4: Multiple parent inputs and sibling outputs
        let template4 = "{{inputs[0]}} and {{inputs[1]}} from {{steps.step1.outputs[0]}} and {{steps.step2.outputs[0]}}";
        let variables4 = vec![
            Value::String("Alice".to_string()),
            Value::String("Bob".to_string())
        ];
        let mut executed_steps4: HashMap<String, ShAction> = HashMap::new();
        let step1_4 = ShAction {
            id: "step1".to_string(),
            name: "step1".to_string(),
            kind: "wasm".to_string(),
            uses: "test-action".to_string(),
            inputs: vec![],
            outputs: vec![ShIO {
                name: "output0".to_string(),
                r#type: "string".to_string(),
                template: Value::String("step1_result".to_string()),
                value: Some(Value::String("step1_result".to_string())),
                required: false,
            }],
            parent_action: None,
            steps: HashMap::new(),
            types: None,
            role: None,
            mirrors: vec![],
            permissions: None,
        };
        let step2_4 = ShAction {
            id: "step2".to_string(),
            name: "step2".to_string(),
            kind: "wasm".to_string(),
            uses: "test-action".to_string(),
            inputs: vec![],
            outputs: vec![ShIO {
                name: "output0".to_string(),
                r#type: "string".to_string(),
                template: Value::String("step2_result".to_string()),
                value: Some(Value::String("step2_result".to_string())),
                required: false,
            }],
            parent_action: None,
            steps: HashMap::new(),
            types: None,
            role: None,
            mirrors: vec![],
            permissions: None,
        };
        executed_steps4.insert("step1".to_string(), step1_4);
        executed_steps4.insert("step2".to_string(), step2_4);
        let result4 = engine.interpolate_string_from_parent_input_or_sibling_output(template4, &variables4, &executed_steps4).unwrap();
        assert_eq!(result4, "Alice and Bob from step1_result and step2_result");
        
        // Test case 5: Parent inputs with JSONPath and sibling outputs
        let template5 = "Name: {{inputs[0].name}} from {{steps.step1.outputs[0]}}";
        let variables5 = vec![Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::String("Charlie".to_string()));
            map
        })];
        let mut executed_steps5: HashMap<String, ShAction> = HashMap::new();
        let step1_5 = ShAction {
            id: "step1".to_string(),
            name: "step1".to_string(),
            kind: "wasm".to_string(),
            uses: "test-action".to_string(),
            inputs: vec![],
            outputs: vec![ShIO {
                name: "output0".to_string(),
                r#type: "string".to_string(),
                template: Value::String("step1_result".to_string()),
                value: Some(Value::String("step1_result".to_string())),
                required: false,
            }],
            parent_action: None,
            steps: HashMap::new(),
            types: None,
            role: None,
            mirrors: vec![],
            permissions: None,
        };
        executed_steps5.insert("step1".to_string(), step1_5);
        let result5 = engine.interpolate_string_from_parent_input_or_sibling_output(template5, &variables5, &executed_steps5).unwrap();
        assert_eq!(result5, "Name: Charlie from step1_result");
        
        // Test case 6: Sibling outputs with JSONPath
        let template6 = "{{inputs[0]}} from {{steps.step1.outputs[0].data}}";
        let variables6 = vec![Value::String("Input".to_string())];
        let mut executed_steps6: HashMap<String, ShAction> = HashMap::new();
        let step1_6 = ShAction {
            id: "step1".to_string(),
            name: "step1".to_string(),
            kind: "wasm".to_string(),
            uses: "test-action".to_string(),
            inputs: vec![],
            outputs: vec![ShIO {
                name: "output0".to_string(),
                r#type: "object".to_string(),
                template: Value::Object({
                    let mut map = serde_json::Map::new();
                    map.insert("data".to_string(), Value::String("step1_data".to_string()));
                    map
                }),
                value: Some(Value::Object({
                    let mut map = serde_json::Map::new();
                    map.insert("data".to_string(), Value::String("step1_data".to_string()));
                    map
                })),
                required: false,
            }],
            parent_action: None,
            steps: HashMap::new(),
            types: None,
            role: None,
            mirrors: vec![],
            permissions: None,
        };
        executed_steps6.insert("step1".to_string(), step1_6);
        let result6 = engine.interpolate_string_from_parent_input_or_sibling_output(template6, &variables6, &executed_steps6).unwrap();
        assert_eq!(result6, "Input from step1_data");
        
        // Test case 7: Non-string values from both sources
        let template7 = "Number {{inputs[0]}} and result {{steps.step1.outputs[0]}}";
        let variables7 = vec![Value::Number(serde_json::Number::from(42))];
        let mut executed_steps7: HashMap<String, ShAction> = HashMap::new();
        let step1_7 = ShAction {
            id: "step1".to_string(),
            name: "step1".to_string(),
            kind: "wasm".to_string(),
            uses: "test-action".to_string(),
            inputs: vec![],
            outputs: vec![ShIO {
                name: "output0".to_string(),
                r#type: "number".to_string(),
                template: Value::Number(serde_json::Number::from(100)),
                value: Some(Value::Number(serde_json::Number::from(100))),
                required: false,
            }],
            parent_action: None,
            steps: HashMap::new(),
            types: None,
            role: None,
            mirrors: vec![],
            permissions: None,
        };
        executed_steps7.insert("step1".to_string(), step1_7);
        let result7 = engine.interpolate_string_from_parent_input_or_sibling_output(template7, &variables7, &executed_steps7).unwrap();
        assert_eq!(result7, "Number 42 and result 100");
        
        // Test case 8: Boolean values from both sources
        let template8 = "Status {{inputs[0]}} and flag {{steps.step1.outputs[0]}}";
        let variables8 = vec![Value::Bool(true)];
        let mut executed_steps8: HashMap<String, ShAction> = HashMap::new();
        let step1_8 = ShAction {
            id: "step1".to_string(),
            name: "step1".to_string(),
            kind: "wasm".to_string(),
            uses: "test-action".to_string(),
            inputs: vec![],
            outputs: vec![ShIO {
                name: "output0".to_string(),
                r#type: "boolean".to_string(),
                template: Value::Bool(false),
                value: Some(Value::Bool(false)),
                required: false,
            }],
            parent_action: None,
            steps: HashMap::new(),
            types: None,
            role: None,
            mirrors: vec![],
            permissions: None,
        };
        executed_steps8.insert("step1".to_string(), step1_8);
        let result8 = engine.interpolate_string_from_parent_input_or_sibling_output(template8, &variables8, &executed_steps8).unwrap();
        assert_eq!(result8, "Status true and flag false");
        
        // Test case 9: Complex mixed template with multiple references
        let template9 = "{{inputs[0]}} {{inputs[1]}} from {{steps.step1.outputs[0]}} and {{steps.step2.outputs[0]}} with {{inputs[0].name}}";
        let variables9 = vec![
            Value::Object({
                let mut map = serde_json::Map::new();
                map.insert("name".to_string(), Value::String("Alice".to_string()));
                map
            }),
            Value::String("Bob".to_string())
        ];
        let mut executed_steps9: HashMap<String, ShAction> = HashMap::new();
        let step1_9 = ShAction {
            id: "step1".to_string(),
            name: "step1".to_string(),
            kind: "wasm".to_string(),
            uses: "test-action".to_string(),
            inputs: vec![],
            outputs: vec![ShIO {
                name: "output0".to_string(),
                r#type: "string".to_string(),
                template: Value::String("step1_result".to_string()),
                value: Some(Value::String("step1_result".to_string())),
                required: false,
            }],
            parent_action: None,
            steps: HashMap::new(),
            types: None,
            role: None,
            mirrors: vec![],
            permissions: None,
        };
        let step2_9 = ShAction {
            id: "step2".to_string(),
            name: "step2".to_string(),
            kind: "wasm".to_string(),
            uses: "test-action".to_string(),
            inputs: vec![],
            outputs: vec![ShIO {
                name: "output0".to_string(),
                r#type: "string".to_string(),
                template: Value::String("step2_result".to_string()),
                value: Some(Value::String("step2_result".to_string())),
                required: false,
            }],
            parent_action: None,
            steps: HashMap::new(),
            types: None,
            role: None,
            mirrors: vec![],
            permissions: None,
        };
        executed_steps9.insert("step1".to_string(), step1_9);
        executed_steps9.insert("step2".to_string(), step2_9);
        let result9 = engine.interpolate_string_from_parent_input_or_sibling_output(template9, &variables9, &executed_steps9).unwrap();
        assert_eq!(result9, "{\"name\":\"Alice\"} Bob from step1_result and step2_result with Alice");
        
        // Test case 10: Empty template
        let template10 = "";
        let variables10 = vec![Value::String("test".to_string())];
        let _executed_steps10: HashMap<String, ShAction> = HashMap::new();
        let result10 = engine.interpolate_string_from_parent_input_or_sibling_output(template10, &variables10, &_executed_steps10).unwrap();
        assert_eq!(result10, "");
    }

    #[test]
    fn test_interpolate() {
        let engine = ExecutionEngine::new();
        
        // Test case 1: String template with simple interpolation
        let template1 = Value::String("Hello {{inputs[0]}} world!".to_string());
        let variables1 = vec![Value::String("John".to_string())];
        let _executed_steps1: HashMap<String, ShAction> = HashMap::new();
        let result1 = engine.interpolate_from_parent_inputs(&template1, &variables1).unwrap();
        assert_eq!(result1, Value::String("Hello John world!".to_string()));
        
        // Test case 2: String template that resolves to JSON object
        let template2 = Value::String("{\"name\": \"{{inputs[0]}}\", \"age\": {{inputs[1]}}}".to_string());
        let variables2 = vec![
            Value::String("Alice".to_string()),
            Value::Number(serde_json::Number::from(25))
        ];
        let _executed_steps2: HashMap<String, ShAction> = HashMap::new();
        let result2 = engine.interpolate_from_parent_inputs(&template2, &variables2).unwrap();
        let expected2 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::String("Alice".to_string()));
            map.insert("age".to_string(), Value::Number(serde_json::Number::from(25)));
            map
        });
        assert_eq!(result2, expected2);
        
        // Test case 3: String template that resolves to JSON array
        let template3 = Value::String("[\"{{inputs[0]}}\", \"{{inputs[1]}}\", \"{{inputs[2]}}\"]".to_string());
        let variables3 = vec![
            Value::String("apple".to_string()),
            Value::String("banana".to_string()),
            Value::String("cherry".to_string())
        ];
        let _executed_steps3: HashMap<String, ShAction> = HashMap::new();
        let result3 = engine.interpolate_from_parent_inputs(&template3, &variables3).unwrap();
        let expected3 = Value::Array(vec![
            Value::String("apple".to_string()),
            Value::String("banana".to_string()),
            Value::String("cherry".to_string())
        ]);
        assert_eq!(result3, expected3);
        
        // Test case 4: String template that resolves to JSON primitive (number)
        let template4 = Value::String("{{inputs[0]}}".to_string());
        let variables4 = vec![Value::Number(serde_json::Number::from(42))];
        let _executed_steps4: HashMap<String, ShAction> = HashMap::new();
        let result4 = engine.interpolate_from_parent_inputs(&template4, &variables4).unwrap();
        assert_eq!(result4, Value::Number(serde_json::Number::from(42)));
        
        // Test case 5: String template that resolves to JSON primitive (boolean)
        let template5 = Value::String("{{inputs[0]}}".to_string());
        let variables5 = vec![Value::Bool(true)];
        let _executed_steps5: HashMap<String, ShAction> = HashMap::new();
        let result5 = engine.interpolate_from_parent_inputs(&template5, &variables5).unwrap();
        assert_eq!(result5, Value::Bool(true));
        
        // Test case 6: String template that resolves to JSON primitive (null)
        let template6 = Value::String("{{inputs[0]}}".to_string());
        let variables6 = vec![Value::Null];
        let _executed_steps6: HashMap<String, ShAction> = HashMap::new();
        let result6 = engine.interpolate_from_parent_inputs(&template6, &variables6).unwrap();
        assert_eq!(result6, Value::Null);
        
        // Test case 7: String template that doesn't resolve to valid JSON
        let template7 = Value::String("Hello {{inputs[0]}} world!".to_string());
        let variables7 = vec![Value::String("John".to_string())];
        let _executed_steps7: HashMap<String, ShAction> = HashMap::new();
        let result7 = engine.interpolate_from_parent_inputs(&template7, &variables7).unwrap();
        assert_eq!(result7, Value::String("Hello John world!".to_string()));
        
        // Test case 8: Object template with string interpolation
        let template8 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::String("{{inputs[0]}}".to_string()));
            map.insert("age".to_string(), Value::String("{{inputs[1]}}".to_string()));
            map.insert("city".to_string(), Value::String("New York".to_string()));
            map
        });
        let variables8 = vec![
            Value::String("Bob".to_string()),
            Value::Number(serde_json::Number::from(30))
        ];
        let _executed_steps8: HashMap<String, ShAction> = HashMap::new();
        let result8 = engine.interpolate_from_parent_inputs(&template8, &variables8).unwrap();
        let expected8 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::String("Bob".to_string()));
            map.insert("age".to_string(), Value::Number(serde_json::Number::from(30)));
            map.insert("city".to_string(), Value::String("New York".to_string()));
            map
        });
        assert_eq!(result8, expected8);
        
        // Test case 9: Object template with nested object interpolation
        let template9 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("user".to_string(), Value::Object({
                let mut user_map = serde_json::Map::new();
                user_map.insert("name".to_string(), Value::String("{{inputs[0]}}".to_string()));
                user_map.insert("profile".to_string(), Value::Object({
                    let mut profile_map = serde_json::Map::new();
                    profile_map.insert("email".to_string(), Value::String("{{inputs[1]}}".to_string()));
                    profile_map
                }));
                user_map
            }));
            map
        });
        let variables9 = vec![
            Value::String("Charlie".to_string()),
            Value::String("charlie@example.com".to_string())
        ];
        let _executed_steps9: HashMap<String, ShAction> = HashMap::new();
        let result9 = engine.interpolate_from_parent_inputs(&template9, &variables9).unwrap();
        let expected9 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("user".to_string(), Value::Object({
                let mut user_map = serde_json::Map::new();
                user_map.insert("name".to_string(), Value::String("Charlie".to_string()));
                user_map.insert("profile".to_string(), Value::Object({
                    let mut profile_map = serde_json::Map::new();
                    profile_map.insert("email".to_string(), Value::String("charlie@example.com".to_string()));
                    profile_map
                }));
                user_map
            }));
            map
        });
        assert_eq!(result9, expected9);
        
        // Test case 10: Array template with string interpolation
        let template10 = Value::Array(vec![
            Value::String("{{inputs[0]}}".to_string()),
            Value::String("{{inputs[1]}}".to_string()),
            Value::String("{{inputs[2]}}".to_string())
        ]);
        let variables10 = vec![
            Value::String("red".to_string()),
            Value::String("green".to_string()),
            Value::String("blue".to_string())
        ];
        let _executed_steps10: HashMap<String, ShAction> = HashMap::new();
        let result10 = engine.interpolate_from_parent_inputs(&template10, &variables10).unwrap();
        let expected10 = Value::Array(vec![
            Value::String("red".to_string()),
            Value::String("green".to_string()),
            Value::String("blue".to_string())
        ]);
        assert_eq!(result10, expected10);
        
        // Test case 11: Array template with mixed types
        let template11 = Value::Array(vec![
            Value::String("{{inputs[0]}}".to_string()),
            Value::Number(serde_json::Number::from(42)),
            Value::Bool(true),
            Value::Null
        ]);
        let variables11 = vec![Value::String("test".to_string())];
        let _executed_steps11: HashMap<String, ShAction> = HashMap::new();
        let result11 = engine.interpolate_from_parent_inputs(&template11, &variables11).unwrap();
        let expected11 = Value::Array(vec![
            Value::String("test".to_string()),
            Value::Number(serde_json::Number::from(42)),
            Value::Bool(true),
            Value::Null
        ]);
        assert_eq!(result11, expected11);
        
        // Test case 12: Array template with nested arrays
        let template12 = Value::Array(vec![
            Value::Array(vec![
                Value::String("{{inputs[0]}}".to_string()),
                Value::String("{{inputs[1]}}".to_string())
            ]),
            Value::Array(vec![
                Value::String("{{inputs[2]}}".to_string()),
                Value::String("{{inputs[3]}}".to_string())
            ])
        ]);
        let variables12 = vec![
            Value::String("a".to_string()),
            Value::String("b".to_string()),
            Value::String("c".to_string()),
            Value::String("d".to_string())
        ];
        let _executed_steps12: HashMap<String, ShAction> = HashMap::new();
        let result12 = engine.interpolate_from_parent_inputs(&template12, &variables12).unwrap();
        let expected12 = Value::Array(vec![
            Value::Array(vec![
                Value::String("a".to_string()),
                Value::String("b".to_string())
            ]),
            Value::Array(vec![
                Value::String("c".to_string()),
                Value::String("d".to_string())
            ])
        ]);
        assert_eq!(result12, expected12);
        
        // Test case 13: Complex nested structure with step interpolation
        let template13 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("user".to_string(), Value::String("{{inputs[0]}}".to_string()));
            map.insert("data".to_string(), Value::Array(vec![
                Value::String("{{steps.step1.outputs[0]}}".to_string()),
                Value::String("{{steps.step2.outputs[0]}}".to_string())
            ]));
            map
        });
        let variables13 = vec![Value::String("David".to_string())];
        let _executed_steps13 = {
            let mut map = HashMap::new();
            let step1 = ShAction {
                id: "step1".to_string(),
                name: "test_step1".to_string(),
                kind: "wasm".to_string(),
                uses: "test/action:1.0.0".to_string(),
                inputs: vec![],
                outputs: vec![
                    ShIO {
                        name: "result1".to_string(),
                        r#type: "string".to_string(),
                        template: Value::String("test_result1".to_string()),
                        value: Some(Value::String("Hello from step1".to_string())),
                        required: true,
                    }
                ],
                parent_action: None,
                steps: HashMap::new(),
                role: None,
                types: None,
                mirrors: vec![],
                permissions: None,
            };
            let step2 = ShAction {
                id: "step2".to_string(),
                name: "test_step2".to_string(),
                kind: "wasm".to_string(),
                uses: "test/action:1.0.0".to_string(),
                inputs: vec![],
                outputs: vec![
                    ShIO {
                        name: "result2".to_string(),
                        r#type: "string".to_string(),
                        template: Value::String("test_result2".to_string()),
                        value: Some(Value::String("Hello from step2".to_string())),
                        required: true,
                    }
                ],
                parent_action: None,
                steps: HashMap::new(),
                role: None,
                types: None,
                mirrors: vec![],
                permissions: None,
            };
            map.insert("step1".to_string(), step1);
            map.insert("step2".to_string(), step2);
            map
        };
        let result13 = engine.interpolate_from_parent_inputs(&template13, &variables13).unwrap();
        let expected13 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("user".to_string(), Value::String("David".to_string()));
            map.insert("data".to_string(), Value::Array(vec![
                Value::String("Hello from step1".to_string()),
                Value::String("Hello from step2".to_string())
            ]));
            map
        });
        assert_eq!(result13, expected13);
        
        // Test case 14: Primitive values (should return unchanged)
        let template14 = Value::Number(serde_json::Number::from(123));
        let variables14 = vec![];
        let result14 = engine.interpolate_from_parent_inputs(&template14, &variables14).unwrap();
        assert_eq!(result14, Value::Number(serde_json::Number::from(123)));
        
        // Test case 15: Boolean primitive (should return unchanged)
        let template15 = Value::Bool(false);
        let variables15 = vec![];
        let _executed_steps15: HashMap<String, ShAction> = HashMap::new();
        let result15 = engine.interpolate_from_parent_inputs(&template15, &variables15).unwrap();
        assert_eq!(result15, Value::Bool(false));
        
        // Test case 16: Null primitive (should return unchanged)
        let template16 = Value::Null;
        let variables16 = vec![];
        let _executed_steps16: HashMap<String, ShAction> = HashMap::new();
        let result16 = engine.interpolate_from_parent_inputs(&template16, &variables16).unwrap();
        assert_eq!(result16, Value::Null);
        
        // Test case 17: String template with JSONPath interpolation
        let template17 = Value::String("{\"name\": \"{{inputs[0].name}}\", \"age\": {{inputs[0].age}}}".to_string());
        let variables17 = vec![Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::String("Eve".to_string()));
            map.insert("age".to_string(), Value::Number(serde_json::Number::from(28)));
            map
        })];
        let _executed_steps17: HashMap<String, ShAction> = HashMap::new();
        let result17 = engine.interpolate_from_parent_inputs(&template17, &variables17).unwrap();
        let expected17 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::String("Eve".to_string()));
            map.insert("age".to_string(), Value::Number(serde_json::Number::from(28)));
            map
        });
        assert_eq!(result17, expected17);
        
        // Test case 18: Empty object (should return unchanged)
        let template18 = Value::Object(serde_json::Map::new());
        let variables18 = vec![];
        let _executed_steps18: HashMap<String, ShAction> = HashMap::new();
        let result18 = engine.interpolate_from_parent_inputs(&template18, &variables18).unwrap();
        assert_eq!(result18, Value::Object(serde_json::Map::new()));
        
        // Test case 19: Empty array (should return unchanged)
        let template19 = Value::Array(vec![]);
        let variables19 = vec![];
        let _executed_steps19: HashMap<String, ShAction> = HashMap::new();
        let result19 = engine.interpolate_from_parent_inputs(&template19, &variables19).unwrap();
        assert_eq!(result19, Value::Array(vec![]));
        
        // Test case 20: String template with malformed JSON (should return as string)
        let template20 = Value::String("Hello {{inputs[0]}} world!".to_string());
        let variables20 = vec![Value::String("Frank".to_string())];
        let _executed_steps20: HashMap<String, ShAction> = HashMap::new();
        let result20 = engine.interpolate_from_parent_inputs(&template20, &variables20).unwrap();
        assert_eq!(result20, Value::String("Hello Frank world!".to_string()));
    }

    #[test]
    fn test_resolve_io() {
        let engine = ExecutionEngine::new();
        
        // Test case 1: Simple IO resolution with string templates
        let io_definitions = vec![
            ShIO {
                name: "input1".to_string(),
                r#type: "string".to_string(),
                template: Value::String("{{inputs[0]}}".to_string()),
                value: None,
                required: true,
            },
            ShIO {
                name: "input2".to_string(),
                r#type: "string".to_string(),
                template: Value::String("{{inputs[1]}}".to_string()),
                value: None,
                required: true,
            }
        ];
        let io_values = vec![
            ShIO {
                name: "input1".to_string(),
                r#type: "string".to_string(),
                template: Value::String("test1".to_string()),
                value: Some(Value::String("Hello".to_string())),
                required: true,
            },
            ShIO {
                name: "input2".to_string(),
                r#type: "string".to_string(),
                template: Value::String("test2".to_string()),
                value: Some(Value::String("World".to_string())),
                required: true,
            }
        ];
        let _steps: HashMap<String, ShAction> = HashMap::new();
        let result = engine.resolve_from_parent_inputs(&io_definitions, &io_values).unwrap();
        let expected = vec![
            Value::String("Hello".to_string()),
            Value::String("World".to_string())
        ];
        assert_eq!(result, expected);
        
        // Test case 2: IO resolution with object templates
        let io_definitions2 = vec![
            ShIO {
                name: "config".to_string(),
                r#type: "object".to_string(),
                template: Value::Object({
                    let mut map = serde_json::Map::new();
                    map.insert("name".to_string(), Value::String("{{inputs[0]}}".to_string()));
                    map.insert("age".to_string(), Value::String("{{inputs[1]}}".to_string()));
                    map
                }),
                value: None,
                required: true,
            }
        ];
        let io_values2 = vec![
            ShIO {
                name: "name".to_string(),
                r#type: "string".to_string(),
                template: Value::String("Alice".to_string()),
                value: Some(Value::String("Alice".to_string())),
                required: true,
            },
            ShIO {
                name: "age".to_string(),
                r#type: "number".to_string(),
                template: Value::String("25".to_string()),
                value: Some(Value::Number(serde_json::Number::from(25))),
                required: true,
            }
        ];
        let result2 = engine.resolve_from_parent_inputs(&io_definitions2, &io_values2).unwrap();
        let expected2 = vec![Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::String("Alice".to_string()));
            map.insert("age".to_string(), Value::Number(serde_json::Number::from(25)));
            map
        })];
        assert_eq!(result2, expected2);
        
        // Test case 3: IO resolution with step dependencies
        let io_definitions3 = vec![
            ShIO {
                name: "result".to_string(),
                r#type: "string".to_string(),
                template: Value::String("{{steps.step1.outputs[0]}}".to_string()),
                value: None,
                required: true,
            }
        ];
        let io_values3 = vec![];
        let _steps3 = {
            let mut map = HashMap::new();
            let step1 = ShAction {
                id: "step1".to_string(),
                name: "test_step".to_string(),
                kind: "wasm".to_string(),
                uses: "test/action:1.0.0".to_string(),
                inputs: vec![],
                outputs: vec![
                    ShIO {
                        name: "output1".to_string(),
                        r#type: "string".to_string(),
                        template: Value::String("test_output".to_string()),
                        value: Some(Value::String("Hello from step1".to_string())),
                        required: true,
                    }
                ],
                parent_action: None,
                steps: HashMap::new(),
                role: None,
                types: None,
                mirrors: vec![],
                permissions: None,
            };
            map.insert("step1".to_string(), step1);
            map
        };
        let result3 = engine.resolve_from_parent_inputs(&io_definitions3, &io_values3).unwrap();
        let expected3 = vec![Value::String("Hello from step1".to_string())];
        assert_eq!(result3, expected3);
        
        // Test case 4: IO resolution with mixed input and step dependencies
        let io_definitions4 = vec![
            ShIO {
                name: "message".to_string(),
                r#type: "string".to_string(),
                template: Value::String("{{inputs[0]}} used {{steps.step1.outputs[0]}}".to_string()),
                value: None,
                required: true,
            }
        ];
        let io_values4 = vec![
            ShIO {
                name: "user".to_string(),
                r#type: "string".to_string(),
                template: Value::String("John".to_string()),
                value: Some(Value::String("John".to_string())),
                required: true,
            }
        ];
        let result4 = engine.resolve_from_parent_inputs(&io_definitions4, &io_values4).unwrap();
        let expected4 = vec![Value::String("John used Hello from step1".to_string())];
        assert_eq!(result4, expected4);
        
        // Test case 5: IO resolution with JSONPath
        let io_definitions5 = vec![
            ShIO {
                name: "name".to_string(),
                r#type: "string".to_string(),
                template: Value::String("{{inputs[0].name}}".to_string()),
                value: None,
                required: true,
            }
        ];
        let io_values5 = vec![
            ShIO {
                name: "user".to_string(),
                r#type: "object".to_string(),
                template: Value::String("user_object".to_string()),
                value: Some(Value::Object({
                    let mut map = serde_json::Map::new();
                    map.insert("name".to_string(), Value::String("Bob".to_string()));
                    map.insert("age".to_string(), Value::Number(serde_json::Number::from(30)));
                    map
                })),
                required: true,
            }
        ];
        let result5 = engine.resolve_from_parent_inputs(&io_definitions5, &io_values5).unwrap();
        let expected5 = vec![Value::String("Bob".to_string())];
        assert_eq!(result5, expected5);
        
        // Test case 6: IO resolution with unresolved templates (should return None)
        let io_definitions6 = vec![
            ShIO {
                name: "result".to_string(),
                r#type: "string".to_string(),
                template: Value::String("{{steps.nonexistent.outputs[0]}}".to_string()),
                value: None,
                required: true,
            }
        ];
        let io_values6 = vec![];
        let result6 = engine.resolve_from_parent_inputs(&io_definitions6, &io_values6);
        assert_eq!(result6, None);
        
        // Test case 7: IO resolution with empty definitions (should return empty vector)
        let io_definitions7 = vec![];
        let io_values7 = vec![];
        let result7 = engine.resolve_from_parent_inputs(&io_definitions7, &io_values7).unwrap();
        assert_eq!(result7, vec![] as Vec<Value>);
        
        // Test case 8: IO resolution with null values
        let io_definitions8 = vec![
            ShIO {
                name: "input1".to_string(),
                r#type: "string".to_string(),
                template: Value::String("{{inputs[0]}}".to_string()),
                value: None,
                required: true,
            }
        ];
        let io_values8 = vec![
            ShIO {
                name: "input1".to_string(),
                r#type: "string".to_string(),
                template: Value::String("test".to_string()),
                value: None, // No value
                required: true,
            }
        ];
        let result8 = engine.resolve_from_parent_inputs(&io_definitions8, &io_values8).unwrap();
        let expected8 = vec![Value::Null];
        assert_eq!(result8, expected8);
        
        // Test case 9: IO resolution with array templates
        let io_definitions9 = vec![
            ShIO {
                name: "items".to_string(),
                r#type: "array".to_string(),
                template: Value::Array(vec![
                    Value::String("{{inputs[0]}}".to_string()),
                    Value::String("{{inputs[1]}}".to_string()),
                    Value::String("{{inputs[2]}}".to_string())
                ]),
                value: None,
                required: true,
            }
        ];
        let io_values9 = vec![
            ShIO {
                name: "item1".to_string(),
                r#type: "string".to_string(),
                template: Value::String("apple".to_string()),
                value: Some(Value::String("apple".to_string())),
                required: true,
            },
            ShIO {
                name: "item2".to_string(),
                r#type: "string".to_string(),
                template: Value::String("banana".to_string()),
                value: Some(Value::String("banana".to_string())),
                required: true,
            },
            ShIO {
                name: "item3".to_string(),
                r#type: "string".to_string(),
                template: Value::String("cherry".to_string()),
                value: Some(Value::String("cherry".to_string())),
                required: true,
            }
        ];
        let result9 = engine.resolve_from_parent_inputs(&io_definitions9, &io_values9).unwrap();
        let expected9 = vec![Value::Array(vec![
            Value::String("apple".to_string()),
            Value::String("banana".to_string()),
            Value::String("cherry".to_string())
        ])];
        assert_eq!(result9, expected9);
        
        // Test case 10: IO resolution with complex nested structure
        let io_definitions10 = vec![
            ShIO {
                name: "config".to_string(),
                r#type: "object".to_string(),
                template: Value::Object({
                    let mut map = serde_json::Map::new();
                    map.insert("user".to_string(), Value::String("{{inputs[0]}}".to_string()));
                    map.insert("data".to_string(), Value::Array(vec![
                        Value::String("{{steps.step1.outputs[0]}}".to_string()),
                        Value::String("{{steps.step2.outputs[0]}}".to_string())
                    ]));
                    map
                }),
                value: None,
                required: true,
            }
        ];
        let io_values10 = vec![
            ShIO {
                name: "user".to_string(),
                r#type: "string".to_string(),
                template: Value::String("Charlie".to_string()),
                value: Some(Value::String("Charlie".to_string())),
                required: true,
            }
        ];
        let _steps10 = {
            let mut map = HashMap::new();
            let step1 = ShAction {
                id: "step1".to_string(),
                name: "test_step1".to_string(),
                kind: "wasm".to_string(),
                uses: "test/action:1.0.0".to_string(),
                inputs: vec![],
                outputs: vec![
                    ShIO {
                        name: "result1".to_string(),
                        r#type: "string".to_string(),
                        template: Value::String("result1".to_string()),
                        value: Some(Value::String("Hello from step1".to_string())),
                        required: true,
                    }
                ],
                parent_action: None,
                steps: HashMap::new(),
                role: None,
                types: None,
                mirrors: vec![],
                permissions: None,
            };
            let step2 = ShAction {
                id: "step2".to_string(),
                name: "test_step2".to_string(),
                kind: "wasm".to_string(),
                uses: "test/action:1.0.0".to_string(),
                inputs: vec![],
                outputs: vec![
                    ShIO {
                        name: "result2".to_string(),
                        r#type: "string".to_string(),
                        template: Value::String("result2".to_string()),
                        value: Some(Value::String("Hello from step2".to_string())),
                        required: true,
                    }
                ],
                parent_action: None,
                steps: HashMap::new(),
                role: None,
                types: None,
                mirrors: vec![],
                permissions: None,
            };
            map.insert("step1".to_string(), step1);
            map.insert("step2".to_string(), step2);
            map
        };
        let result10 = engine.resolve_from_parent_inputs(&io_definitions10, &io_values10).unwrap();
        let expected10 = vec![Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("user".to_string(), Value::String("Charlie".to_string()));
            map.insert("data".to_string(), Value::Array(vec![
                Value::String("Hello from step1".to_string()),
                Value::String("Hello from step2".to_string())
            ]));
            map
        })];
        assert_eq!(result10, expected10);
        
        // Test case 11: IO resolution with interpolation failure (should return None)
        let io_definitions11 = vec![
            ShIO {
                name: "result".to_string(),
                r#type: "string".to_string(),
                template: Value::String("{{inputs[0]}}".to_string()),
                value: None,
                required: true,
            }
        ];
        let io_values11 = vec![]; // No values provided
        let result11 = engine.resolve_from_parent_inputs(&io_definitions11, &io_values11);
        assert_eq!(result11, None);
        
        // Test case 12: IO resolution with still unresolved templates (should return None)
        let io_definitions12 = vec![
            ShIO {
                name: "result".to_string(),
                r#type: "string".to_string(),
                template: Value::String("{{steps.step1.outputs[0]}}".to_string()),
                value: None,
                required: true,
            }
        ];
        let io_values12 = vec![];
        let _steps12: HashMap<String, ShAction> = HashMap::new(); // No executed steps
        let result12 = engine.resolve_from_parent_inputs(&io_definitions12, &io_values12);
        assert_eq!(result12, None);
    }

    #[test]
    fn test_convert_to_json_schema() {
        let engine = ExecutionEngine::new();
        
        // Test case 1: Simple primitive type (string)
        let type_def1 = Value::String("string".to_string());
        let result1 = engine.convert_to_json_schema(&type_def1).unwrap();
        let expected1 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("type".to_string(), Value::String("string".to_string()));
            map
        });
        assert_eq!(result1, expected1);
        
        // Test case 2: Simple primitive type (number)
        let type_def2 = Value::String("number".to_string());
        let result2 = engine.convert_to_json_schema(&type_def2).unwrap();
        let expected2 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("type".to_string(), Value::String("number".to_string()));
            map
        });
        assert_eq!(result2, expected2);
        
        // Test case 3: Simple primitive type (boolean)
        let type_def3 = Value::String("boolean".to_string());
        let result3 = engine.convert_to_json_schema(&type_def3).unwrap();
        let expected3 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("type".to_string(), Value::String("boolean".to_string()));
            map
        });
        assert_eq!(result3, expected3);
        
        // Test case 4: Special "object" type (accepts any JSON value)
        let type_def4 = Value::String("object".to_string());
        let result4 = engine.convert_to_json_schema(&type_def4).unwrap();
        let expected4 = Value::Object(serde_json::Map::new());
        assert_eq!(result4, expected4);
        
        // Test case 5: Array type definition
        let type_def5 = Value::Array(vec![
            Value::String("string".to_string())
        ]);
        let result5 = engine.convert_to_json_schema(&type_def5).unwrap();
        let expected5 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("type".to_string(), Value::String("array".to_string()));
            map.insert("items".to_string(), Value::Object({
                let mut items_map = serde_json::Map::new();
                items_map.insert("type".to_string(), Value::String("string".to_string()));
                items_map
            }));
            map
        });
        assert_eq!(result5, expected5);
        
        // Test case 6: Field definition with type and description
        let type_def6 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("type".to_string(), Value::String("string".to_string()));
            map.insert("description".to_string(), Value::String("User's name".to_string()));
            map
        });
        let result6 = engine.convert_to_json_schema(&type_def6).unwrap();
        let expected6 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("type".to_string(), Value::String("string".to_string()));
            map.insert("description".to_string(), Value::String("User's name".to_string()));
            map
        });
        assert_eq!(result6, expected6);
        
        // Test case 7: Field definition with nested properties
        let type_def7 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("type".to_string(), Value::String("object".to_string()));
            map.insert("properties".to_string(), Value::Object({
                let mut props_map = serde_json::Map::new();
                props_map.insert("name".to_string(), Value::Object({
                    let mut name_map = serde_json::Map::new();
                    name_map.insert("type".to_string(), Value::String("string".to_string()));
                    name_map
                }));
                props_map
            }));
            map
        });
        let result7 = engine.convert_to_json_schema(&type_def7).unwrap();
        let expected7 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("type".to_string(), Value::String("object".to_string()));
            map.insert("properties".to_string(), Value::Object({
                let mut props_map = serde_json::Map::new();
                props_map.insert("type".to_string(), Value::String("object".to_string()));
                props_map.insert("additionalProperties".to_string(), Value::Bool(false));
                props_map.insert("properties".to_string(), Value::Object({
                    let mut nested_props_map = serde_json::Map::new();
                    nested_props_map.insert("name".to_string(), Value::Object({
                        let mut name_map = serde_json::Map::new();
                        name_map.insert("type".to_string(), Value::String("string".to_string()));
                        name_map
                    }));
                    nested_props_map
                }));
                props_map
            }));
            map
        });
        assert_eq!(result7, expected7);
        
        // Test case 8: Field definition with array items
        let type_def8 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("type".to_string(), Value::String("array".to_string()));
            map.insert("items".to_string(), Value::Object({
                let mut items_map = serde_json::Map::new();
                items_map.insert("type".to_string(), Value::String("string".to_string()));
                items_map
            }));
            map
        });
        let result8 = engine.convert_to_json_schema(&type_def8).unwrap();
        let expected8 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("type".to_string(), Value::String("array".to_string()));
            map.insert("items".to_string(), Value::Object({
                let mut items_map = serde_json::Map::new();
                items_map.insert("type".to_string(), Value::String("string".to_string()));
                items_map
            }));
            map
        });
        assert_eq!(result8, expected8);
        
        // Test case 9: Type definition with multiple fields (object schema)
        let type_def9 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::Object({
                let mut name_map = serde_json::Map::new();
                name_map.insert("type".to_string(), Value::String("string".to_string()));
                name_map.insert("required".to_string(), Value::Bool(true));
                name_map
            }));
            map.insert("age".to_string(), Value::Object({
                let mut age_map = serde_json::Map::new();
                age_map.insert("type".to_string(), Value::String("number".to_string()));
                age_map.insert("required".to_string(), Value::Bool(false));
                age_map
            }));
            map
        });
        let result9 = engine.convert_to_json_schema(&type_def9).unwrap();
        let expected9 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("type".to_string(), Value::String("object".to_string()));
            map.insert("additionalProperties".to_string(), Value::Bool(false));
            map.insert("properties".to_string(), Value::Object({
                let mut props_map = serde_json::Map::new();
                props_map.insert("name".to_string(), Value::Object({
                    let mut name_map = serde_json::Map::new();
                    name_map.insert("type".to_string(), Value::String("string".to_string()));
                    name_map
                }));
                props_map.insert("age".to_string(), Value::Object({
                    let mut age_map = serde_json::Map::new();
                    age_map.insert("type".to_string(), Value::String("number".to_string()));
                    age_map
                }));
                props_map
            }));
            map.insert("required".to_string(), Value::Array(vec![Value::String("name".to_string())]));
            map
        });
        assert_eq!(result9, expected9);
        
        // Test case 10: Type definition with all required fields
        let type_def10 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("id".to_string(), Value::Object({
                let mut id_map = serde_json::Map::new();
                id_map.insert("type".to_string(), Value::String("string".to_string()));
                id_map.insert("required".to_string(), Value::Bool(true));
                id_map
            }));
            map.insert("email".to_string(), Value::Object({
                let mut email_map = serde_json::Map::new();
                email_map.insert("type".to_string(), Value::String("string".to_string()));
                email_map.insert("required".to_string(), Value::Bool(true));
                email_map
            }));
            map.insert("active".to_string(), Value::Object({
                let mut active_map = serde_json::Map::new();
                active_map.insert("type".to_string(), Value::String("boolean".to_string()));
                active_map.insert("required".to_string(), Value::Bool(true));
                active_map
            }));
            map
        });
        let result10 = engine.convert_to_json_schema(&type_def10).unwrap();
        let expected10 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("type".to_string(), Value::String("object".to_string()));
            map.insert("additionalProperties".to_string(), Value::Bool(false));
            map.insert("properties".to_string(), Value::Object({
                let mut props_map = serde_json::Map::new();
                props_map.insert("id".to_string(), Value::Object({
                    let mut id_map = serde_json::Map::new();
                    id_map.insert("type".to_string(), Value::String("string".to_string()));
                    id_map
                }));
                props_map.insert("email".to_string(), Value::Object({
                    let mut email_map = serde_json::Map::new();
                    email_map.insert("type".to_string(), Value::String("string".to_string()));
                    email_map
                }));
                props_map.insert("active".to_string(), Value::Object({
                    let mut active_map = serde_json::Map::new();
                    active_map.insert("type".to_string(), Value::String("boolean".to_string()));
                    active_map
                }));
                props_map
            }));
            map.insert("required".to_string(), Value::Array(vec![
                Value::String("active".to_string()),
                Value::String("email".to_string()),
                Value::String("id".to_string())
            ]));
            map
        });
        assert_eq!(result10, expected10);
        
        // Test case 11: Type definition with no required fields
        let type_def11 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("optional_field".to_string(), Value::Object({
                let mut field_map = serde_json::Map::new();
                field_map.insert("type".to_string(), Value::String("string".to_string()));
                field_map.insert("required".to_string(), Value::Bool(false));
                field_map
            }));
            map
        });
        let result11 = engine.convert_to_json_schema(&type_def11).unwrap();
        let expected11 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("type".to_string(), Value::String("object".to_string()));
            map.insert("additionalProperties".to_string(), Value::Bool(false));
            map.insert("properties".to_string(), Value::Object({
                let mut props_map = serde_json::Map::new();
                props_map.insert("optional_field".to_string(), Value::Object({
                    let mut field_map = serde_json::Map::new();
                    field_map.insert("type".to_string(), Value::String("string".to_string()));
                    field_map
                }));
                props_map
            }));
            map
        });
        assert_eq!(result11, expected11);
        
        // Test case 12: Complex nested object with arrays
        let type_def12 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("user".to_string(), Value::Object({
                let mut user_map = serde_json::Map::new();
                user_map.insert("type".to_string(), Value::String("object".to_string()));
                user_map.insert("properties".to_string(), Value::Object({
                    let mut props_map = serde_json::Map::new();
                    props_map.insert("name".to_string(), Value::Object({
                        let mut name_map = serde_json::Map::new();
                        name_map.insert("type".to_string(), Value::String("string".to_string()));
                        name_map
                    }));
                    props_map
                }));
                user_map.insert("required".to_string(), Value::Bool(true));
                user_map
            }));
            map.insert("tags".to_string(), Value::Object({
                let mut tags_map = serde_json::Map::new();
                tags_map.insert("type".to_string(), Value::String("array".to_string()));
                tags_map.insert("items".to_string(), Value::Object({
                    let mut items_map = serde_json::Map::new();
                    items_map.insert("type".to_string(), Value::String("string".to_string()));
                    items_map
                }));
                tags_map.insert("required".to_string(), Value::Bool(false));
                tags_map
            }));
            map
        });
        let result12 = engine.convert_to_json_schema(&type_def12).unwrap();
        let expected12 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("type".to_string(), Value::String("object".to_string()));
            map.insert("additionalProperties".to_string(), Value::Bool(false));
            map.insert("properties".to_string(), Value::Object({
                let mut props_map = serde_json::Map::new();
                props_map.insert("user".to_string(), Value::Object({
                    let mut user_map = serde_json::Map::new();
                    user_map.insert("type".to_string(), Value::String("object".to_string()));
                    user_map.insert("properties".to_string(), Value::Object({
                        let mut nested_props_map = serde_json::Map::new();
                        nested_props_map.insert("type".to_string(), Value::String("object".to_string()));
                        nested_props_map.insert("additionalProperties".to_string(), Value::Bool(false));
                        nested_props_map.insert("properties".to_string(), Value::Object({
                            let mut name_props_map = serde_json::Map::new();
                            name_props_map.insert("name".to_string(), Value::Object({
                                let mut name_map = serde_json::Map::new();
                                name_map.insert("type".to_string(), Value::String("string".to_string()));
                                name_map
                            }));
                            name_props_map
                        }));
                        nested_props_map
                    }));
                    user_map
                }));
                props_map.insert("tags".to_string(), Value::Object({
                    let mut tags_map = serde_json::Map::new();
                    tags_map.insert("type".to_string(), Value::String("array".to_string()));
                    tags_map.insert("items".to_string(), Value::Object({
                        let mut items_map = serde_json::Map::new();
                        items_map.insert("type".to_string(), Value::String("string".to_string()));
                        items_map
                    }));
                    tags_map
                }));
                props_map
            }));
            map.insert("required".to_string(), Value::Array(vec![Value::String("user".to_string())]));
            map
        });
        assert_eq!(result12, expected12);
        
        // Test case 13: Unsupported type definition format (number)
        let type_def13 = Value::Number(serde_json::Number::from(42));
        let result13 = engine.convert_to_json_schema(&type_def13);
        assert!(result13.is_err());
        
        // Test case 14: Unsupported type definition format (boolean)
        let type_def14 = Value::Bool(true);
        let result14 = engine.convert_to_json_schema(&type_def14);
        assert!(result14.is_err());
        
        // Test case 15: Unsupported type definition format (null)
        let type_def15 = Value::Null;
        let result15 = engine.convert_to_json_schema(&type_def15);
        assert!(result15.is_err());
        
        // Test case 16: Verify field order preservation
        let type_def16 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("first".to_string(), Value::Object({
                let mut first_map = serde_json::Map::new();
                first_map.insert("type".to_string(), Value::String("string".to_string()));
                first_map.insert("required".to_string(), Value::Bool(true));
                first_map
            }));
            map.insert("second".to_string(), Value::Object({
                let mut second_map = serde_json::Map::new();
                second_map.insert("type".to_string(), Value::String("string".to_string()));
                second_map.insert("required".to_string(), Value::Bool(true));
                second_map
            }));
            map.insert("third".to_string(), Value::Object({
                let mut third_map = serde_json::Map::new();
                third_map.insert("type".to_string(), Value::String("string".to_string()));
                third_map.insert("required".to_string(), Value::Bool(true));
                third_map
            }));
            map
        });
        let result16 = engine.convert_to_json_schema(&type_def16).unwrap();
        
        // The required array should contain fields in the order they were defined
        if let Value::Object(schema) = result16 {
            if let Some(Value::Array(required)) = schema.get("required") {
                // Check that the required fields are in the expected order
                assert_eq!(required.len(), 3);
                // With the updated implementation, the order should be preserved
                let required_strings: Vec<String> = required.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                assert_eq!(required_strings[0], "first");
                assert_eq!(required_strings[1], "second");
                assert_eq!(required_strings[2], "third");
            } else {
                panic!("Required array not found in schema");
            }
        } else {
            panic!("Schema is not an object");
        }
    }

    #[tokio::test]
    async fn test_evaluate_jsonpath() {
        let engine = ExecutionEngine::new();
        
        // Test case 1: Simple object key access
        let value1 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("name".to_string(), Value::String("John".to_string()));
            map.insert("age".to_string(), Value::Number(30.into()));
            map
        });
        let result1 = engine.evaluate_jsonpath(&value1, "name").unwrap();
        assert_eq!(result1, Value::String("John".to_string()));
        
        // Test case 2: Numeric value access
        let result2 = engine.evaluate_jsonpath(&value1, "age").unwrap();
        assert_eq!(result2, Value::Number(30.into()));
        
        // Test case 3: Nested object access
        let value3 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("user".to_string(), Value::Object({
                let mut user_map = serde_json::Map::new();
                user_map.insert("profile".to_string(), Value::Object({
                    let mut profile_map = serde_json::Map::new();
                    profile_map.insert("name".to_string(), Value::String("Alice".to_string()));
                    profile_map.insert("email".to_string(), Value::String("alice@example.com".to_string()));
                    profile_map
                }));
                user_map
            }));
            map
        });
        let result3 = engine.evaluate_jsonpath(&value3, "user.profile.name").unwrap();
        assert_eq!(result3, Value::String("Alice".to_string()));
        
        // Test case 4: Array index access
        let value4 = Value::Array(vec![
            Value::String("first".to_string()),
            Value::String("second".to_string()),
            Value::String("third".to_string())
        ]);
        let result4 = engine.evaluate_jsonpath(&value4, "1").unwrap();
        assert_eq!(result4, Value::String("second".to_string()));
        
        // Test case 5: Object with array property access
        let value5 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("items".to_string(), Value::Array(vec![
                Value::String("item1".to_string()),
                Value::String("item2".to_string()),
                Value::String("item3".to_string())
            ]));
            map
        });
        let result5 = engine.evaluate_jsonpath(&value5, "items[1]").unwrap();
        assert_eq!(result5, Value::String("item2".to_string()));
        
        // Test case 6: Nested object with array access
        let value6 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("data".to_string(), Value::Object({
                let mut data_map = serde_json::Map::new();
                data_map.insert("users".to_string(), Value::Array(vec![
                    Value::Object({
                        let mut user1_map = serde_json::Map::new();
                        user1_map.insert("name".to_string(), Value::String("User1".to_string()));
                        user1_map.insert("id".to_string(), Value::Number(1.into()));
                        user1_map
                    }),
                    Value::Object({
                        let mut user2_map = serde_json::Map::new();
                        user2_map.insert("name".to_string(), Value::String("User2".to_string()));
                        user2_map.insert("id".to_string(), Value::Number(2.into()));
                        user2_map
                    })
                ]));
                data_map
            }));
            map
        });
        let result6 = engine.evaluate_jsonpath(&value6, "data.users[0].name").unwrap();
        assert_eq!(result6, Value::String("User1".to_string()));
        
        // Test case 7: Boolean value access
        let value7 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("active".to_string(), Value::Bool(true));
            map.insert("verified".to_string(), Value::Bool(false));
            map
        });
        let result7 = engine.evaluate_jsonpath(&value7, "active").unwrap();
        assert_eq!(result7, Value::Bool(true));
        
        // Test case 8: Null value access
        let value8 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("description".to_string(), Value::Null);
            map.insert("title".to_string(), Value::String("Test".to_string()));
            map
        });
        let result8 = engine.evaluate_jsonpath(&value8, "description").unwrap();
        assert_eq!(result8, Value::Null);
        
        // Test case 9: Array of objects access
        let value9 = Value::Array(vec![
            Value::Object({
                let mut obj1_map = serde_json::Map::new();
                obj1_map.insert("type".to_string(), Value::String("admin".to_string()));
                obj1_map.insert("level".to_string(), Value::Number(5.into()));
                obj1_map
            }),
            Value::Object({
                let mut obj2_map = serde_json::Map::new();
                obj2_map.insert("type".to_string(), Value::String("user".to_string()));
                obj2_map.insert("level".to_string(), Value::Number(1.into()));
                obj2_map
            })
        ]);
        let result9 = engine.evaluate_jsonpath(&value9, "0.type").unwrap();
        assert_eq!(result9, Value::String("admin".to_string()));
        
        // Test case 10: Complex nested structure
        let value10 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("response".to_string(), Value::Object({
                let mut response_map = serde_json::Map::new();
                response_map.insert("data".to_string(), Value::Array(vec![
                    Value::Object({
                        let mut item_map = serde_json::Map::new();
                        item_map.insert("id".to_string(), Value::Number(1.into()));
                        item_map.insert("metadata".to_string(), Value::Object({
                            let mut meta_map = serde_json::Map::new();
                            meta_map.insert("tags".to_string(), Value::Array(vec![
                                Value::String("tag1".to_string()),
                                Value::String("tag2".to_string())
                            ]));
                            meta_map
                        }));
                        item_map
                    })
                ]));
                response_map
            }));
            map
        });
        let result10 = engine.evaluate_jsonpath(&value10, "response.data[0].metadata.tags[1]").unwrap();
        assert_eq!(result10, Value::String("tag2".to_string()));
        
        // Test case 11: Error - path not found in object
        let result11 = engine.evaluate_jsonpath(&value1, "nonexistent");
        assert!(result11.is_err());
        assert!(result11.unwrap_err().to_string().contains("Path 'nonexistent' not found in object"));
        
        // Test case 12: Error - index out of bounds
        let result12 = engine.evaluate_jsonpath(&value4, "5");
        assert!(result12.is_err());
        assert!(result12.unwrap_err().to_string().contains("Index 5 out of bounds in array"));
        
        // Test case 13: Error - invalid array index
        let result13 = engine.evaluate_jsonpath(&value4, "invalid");
        assert!(result13.is_err());
        assert!(result13.unwrap_err().to_string().contains("Invalid array index: invalid"));
        
        // Test case 14: Error - accessing object key on non-object
        let result14 = engine.evaluate_jsonpath(&value4, "name");
        assert!(result14.is_err());
        assert!(result14.unwrap_err().to_string().contains("Invalid array index: name"));
        
        // Test case 15: Error - accessing array index on non-array
        let result15 = engine.evaluate_jsonpath(&value1, "0");
        assert!(result15.is_err());
        assert!(result15.unwrap_err().to_string().contains("Path '0' not found in object"));
        
        // Test case 16: Error - invalid array index in bracket notation
        let result16 = engine.evaluate_jsonpath(&value5, "items[invalid]");
        assert!(result16.is_err());
        assert!(result16.unwrap_err().to_string().contains("Invalid array index: invalid"));
        
        // Test case 17: Error - accessing non-object with bracket notation
        let result17 = engine.evaluate_jsonpath(&value4, "items[0]");
        assert!(result17.is_err());
        assert!(result17.unwrap_err().to_string().contains("Cannot access 'items' on non-object"));
        
        // Test case 18: Error - accessing non-array after object key
        let value18 = Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("items".to_string(), Value::String("not_an_array".to_string()));
            map
        });
        let result18 = engine.evaluate_jsonpath(&value18, "items[0]");
        assert!(result18.is_err());
        assert!(result18.unwrap_err().to_string().contains("Cannot access array index on non-array"));
        
        // Test case 19: Empty path (should return the original value)
        let result19 = engine.evaluate_jsonpath(&value1, "").unwrap();
        assert_eq!(result19, value1);
        
        // Test case 20: Single dot path (should return the original value)
        let result20 = engine.evaluate_jsonpath(&value1, ".").unwrap();
        assert_eq!(result20, value1);
        
        // Test case 21: Path with multiple consecutive dots (should skip empty parts)
        let result21 = engine.evaluate_jsonpath(&value3, "user..profile.name").unwrap();
        assert_eq!(result21, Value::String("Alice".to_string()));
    }

    #[tokio::test]
    async fn test_cast() {
        let engine = ExecutionEngine::new();
        
        // Test case 1: Primitive string type
        let types1 = None;
        let io_definitions1 = vec![
            ShIO {
                name: "name".to_string(),
                r#type: "string".to_string(),
                template: Value::String("John".to_string()),
                value: None,
                required: true,
            }
        ];
        let io_values1 = vec![Value::String("John".to_string())];
        let result1 = engine.cast(&types1, &io_definitions1, &io_values1, None).unwrap();
        assert_eq!(result1.len(), 1);
        assert_eq!(result1[0], Value::String("John".to_string()));
        
        // Test case 2: Primitive boolean type
        let io_definitions2 = vec![
            ShIO {
                name: "active".to_string(),
                r#type: "bool".to_string(),
                template: Value::Bool(true),
                value: None,
                required: true,
            }
        ];
        let io_values2 = vec![Value::Bool(true)];
        let result2 = engine.cast(&types1, &io_definitions2, &io_values2, None).unwrap();
        assert_eq!(result2.len(), 1);
        assert_eq!(result2[0], Value::Bool(true));
        
        // Test case 3: Primitive number type
        let io_definitions3 = vec![
            ShIO {
                name: "age".to_string(),
                r#type: "number".to_string(),
                template: Value::Number(30.into()),
                value: None,
                required: true,
            }
        ];
        let io_values3 = vec![Value::Number(30.into())];
        let result3 = engine.cast(&types1, &io_definitions3, &io_values3, None).unwrap();
        assert_eq!(result3.len(), 1);
        assert_eq!(result3[0], Value::Number(30.into()));
        
        // Test case 4: Primitive object type
        let io_definitions4 = vec![
            ShIO {
                name: "data".to_string(),
                r#type: "object".to_string(),
                template: Value::Object(serde_json::Map::new()),
                value: None,
                required: true,
            }
        ];
        let io_values4 = vec![Value::Object({
            let mut map = serde_json::Map::new();
            map.insert("key".to_string(), Value::String("value".to_string()));
            map
        })];
        let result4 = engine.cast(&types1, &io_definitions4, &io_values4, None).unwrap();
        assert_eq!(result4.len(), 1);
        assert_eq!(result4[0], io_values4[0]);
        
        // Test case 5: Multiple primitive types
        let io_definitions5 = vec![
            ShIO {
                name: "name".to_string(),
                r#type: "string".to_string(),
                template: Value::String("Alice".to_string()),
                value: None,
                required: true,
            },
            ShIO {
                name: "age".to_string(),
                r#type: "number".to_string(),
                template: Value::Number(25.into()),
                value: None,
                required: true,
            },
            ShIO {
                name: "active".to_string(),
                r#type: "bool".to_string(),
                template: Value::Bool(false),
                value: None,
                required: true,
            }
        ];
        let io_values5 = vec![
            Value::String("Alice".to_string()),
            Value::Number(25.into()),
            Value::Bool(false)
        ];
        let result5 = engine.cast(&types1, &io_definitions5, &io_values5, None).unwrap();
        assert_eq!(result5.len(), 3);
        assert_eq!(result5[0], Value::String("Alice".to_string()));
        assert_eq!(result5[1], Value::Number(25.into()));
        assert_eq!(result5[2], Value::Bool(false));
        
        // Test case 6: Custom type with valid value
        let types6 = Some({
            let mut map = serde_json::Map::new();
            map.insert("User".to_string(), Value::Object({
                let mut user_type = serde_json::Map::new();
                user_type.insert("name".to_string(), Value::Object({
                    let mut name_field = serde_json::Map::new();
                    name_field.insert("type".to_string(), Value::String("string".to_string()));
                    name_field.insert("required".to_string(), Value::Bool(true));
                    name_field
                }));
                user_type.insert("age".to_string(), Value::Object({
                    let mut age_field = serde_json::Map::new();
                    age_field.insert("type".to_string(), Value::String("number".to_string()));
                    age_field.insert("required".to_string(), Value::Bool(true));
                    age_field
                }));
                user_type
            }));
            map
        });
        let io_definitions6 = vec![
            ShIO {
                name: "user".to_string(),
                r#type: "User".to_string(),
                template: Value::Object(serde_json::Map::new()),
                value: None,
                required: true,
            }
        ];
        let io_values6 = vec![Value::Object({
            let mut user_obj = serde_json::Map::new();
            user_obj.insert("name".to_string(), Value::String("Bob".to_string()));
            user_obj.insert("age".to_string(), Value::Number(30.into()));
            user_obj
        })];
        let result6 = engine.cast(&types6, &io_definitions6, &io_values6, None).unwrap();
        assert_eq!(result6.len(), 1);
        assert_eq!(result6[0], io_values6[0]);
        
        // Test case 7: Custom type with invalid value (missing required field)
        let io_values7 = vec![Value::Object({
            let mut user_obj = serde_json::Map::new();
            user_obj.insert("name".to_string(), Value::String("Bob".to_string()));
            // Missing required "age" field
            user_obj
        })];
        let result7 = engine.cast(&types6, &io_definitions6, &io_values7, None);
        assert!(result7.is_err());
        assert!(result7.unwrap_err().to_string().contains("Value 0 is invalid"));
        
        // Test case 8: Custom type with invalid value (wrong type)
        let io_values8 = vec![Value::Object({
            let mut user_obj = serde_json::Map::new();
            user_obj.insert("name".to_string(), Value::Number(123.into())); // Should be string
            user_obj.insert("age".to_string(), Value::Number(30.into()));
            user_obj
        })];
        let result8 = engine.cast(&types6, &io_definitions6, &io_values8, None);
        assert!(result8.is_err());
        assert!(result8.unwrap_err().to_string().contains("Value 0 is invalid"));
        
        // Test case 9: Custom type with additional properties (should fail due to strict validation)
        let io_values9 = vec![Value::Object({
            let mut user_obj = serde_json::Map::new();
            user_obj.insert("name".to_string(), Value::String("Bob".to_string()));
            user_obj.insert("age".to_string(), Value::Number(30.into()));
            user_obj.insert("extra".to_string(), Value::String("not allowed".to_string())); // Additional property
            user_obj
        })];
        let result9 = engine.cast(&types6, &io_definitions6, &io_values9, None);
        assert!(result9.is_err());
        assert!(result9.unwrap_err().to_string().contains("Value 0 is invalid"));
        
        // Test case 10: Mixed primitive and custom types
        let io_definitions10 = vec![
            ShIO {
                name: "title".to_string(),
                r#type: "string".to_string(),
                template: Value::String("Test".to_string()),
                value: None,
                required: true,
            },
            ShIO {
                name: "user".to_string(),
                r#type: "User".to_string(),
                template: Value::Object(serde_json::Map::new()),
                value: None,
                required: true,
            }
        ];
        let io_values10 = vec![
            Value::String("Test Title".to_string()),
            Value::Object({
                let mut user_obj = serde_json::Map::new();
                user_obj.insert("name".to_string(), Value::String("Alice".to_string()));
                user_obj.insert("age".to_string(), Value::Number(25.into()));
                user_obj
            })
        ];
        let result10 = engine.cast(&types6, &io_definitions10, &io_values10, None).unwrap();
        assert_eq!(result10.len(), 2);
        assert_eq!(result10[0], Value::String("Test Title".to_string()));
        assert_eq!(result10[1], io_values10[1]);
        
        // Test case 11: Custom type with array
        let types11 = Some({
            let mut map = serde_json::Map::new();
            map.insert("UserList".to_string(), Value::Array(vec![
                Value::Object({
                    let mut user_type = serde_json::Map::new();
                    user_type.insert("name".to_string(), Value::Object({
                        let mut name_field = serde_json::Map::new();
                        name_field.insert("type".to_string(), Value::String("string".to_string()));
                        name_field.insert("required".to_string(), Value::Bool(true));
                        name_field
                    }));
                    user_type
                })
            ]));
            map
        });
        let io_definitions11 = vec![
            ShIO {
                name: "users".to_string(),
                r#type: "UserList".to_string(),
                template: Value::Array(vec![]),
                value: None,
                required: true,
            }
        ];
        let io_values11 = vec![Value::Array(vec![
            Value::Object({
                let mut user_obj = serde_json::Map::new();
                user_obj.insert("name".to_string(), Value::String("User1".to_string()));
                user_obj
            }),
            Value::Object({
                let mut user_obj = serde_json::Map::new();
                user_obj.insert("name".to_string(), Value::String("User2".to_string()));
                user_obj
            })
        ])];
        let result11 = engine.cast(&types11, &io_definitions11, &io_values11, None).unwrap();
        assert_eq!(result11.len(), 1);
        assert_eq!(result11[0], io_values11[0]);
        
        // Test case 12: Type definition not found (should pass through unchanged)
        let io_definitions12 = vec![
            ShIO {
                name: "unknown".to_string(),
                r#type: "UnknownType".to_string(),
                template: Value::String("test".to_string()),
                value: None,
                required: true,
            }
        ];
        let io_values12 = vec![Value::String("test".to_string())];
        let result12 = engine.cast(&types6, &io_definitions12, &io_values12, None).unwrap();
        // Should pass through unchanged since UnknownType is not in types map
        assert_eq!(result12.len(), 1);
        assert_eq!(result12[0], Value::String("test".to_string()));
        
        // Test case 13: Error - invalid type definition
        let types13 = Some({
            let mut map = serde_json::Map::new();
            map.insert("InvalidType".to_string(), Value::String("invalid".to_string())); // Invalid type definition
            map
        });
        let io_definitions13 = vec![
            ShIO {
                name: "invalid".to_string(),
                r#type: "InvalidType".to_string(),
                template: Value::String("test".to_string()),
                value: None,
                required: true,
            }
        ];
        let io_values13 = vec![Value::String("test".to_string())];
        let result13 = engine.cast(&types13, &io_definitions13, &io_values13, None);
        assert!(result13.is_err());
        assert!(result13.unwrap_err().to_string().contains("Failed to compile schema for type 'InvalidType'"));
        
        // Test case 14: Empty IO definitions and values
        let io_definitions14 = vec![];
        let io_values14 = vec![];
        let result14 = engine.cast(&types1, &io_definitions14, &io_values14, None).unwrap();
        assert_eq!(result14.len(), 0);
        
        // Test case 15: Mismatched IO definitions and values length (should panic)
        let io_definitions15 = vec![
            ShIO {
                name: "name".to_string(),
                r#type: "string".to_string(),
                template: Value::String("test".to_string()),
                value: None,
                required: true,
            }
        ];
        let io_values15 = vec![]; // Empty values
        // This should panic due to index out of bounds
        let result15 = std::panic::catch_unwind(|| {
            engine.cast(&types1, &io_definitions15, &io_values15, None)
        });
        assert!(result15.is_err()); // Should panic on unwrap() when getting value by index
    }

    #[tokio::test]
    async fn test_inject_values() {
        let engine = ExecutionEngine::new();
        
        // Test case 1: Basic primitive type injection
        let io_fields1 = vec![
            ShIO {
                name: "name".to_string(),
                r#type: "string".to_string(),
                template: Value::String("John".to_string()),
                value: None,
                required: true,
            },
            ShIO {
                name: "age".to_string(),
                r#type: "number".to_string(),
                template: Value::Number(30.into()),
                value: None,
                required: true,
            }
        ];
        let input_values1 = vec![
            Value::String("Alice".to_string()),
            Value::Number(25.into())
        ];
        let types1 = None;
        
        let result1 = engine.instantiate_io(&io_fields1, &input_values1, &types1, None);
        assert!(result1.is_ok());
        
        // Check that values were injected
        let injected_fields1 = result1.unwrap();
        assert_eq!(injected_fields1[0].value, Some(Value::String("Alice".to_string())));
        assert_eq!(injected_fields1[1].value, Some(Value::Number(25.into())));
        
        // Test case 2: Boolean and object type injection
        let io_fields2 = vec![
            ShIO {
                name: "active".to_string(),
                r#type: "bool".to_string(),
                template: Value::Bool(true),
                value: None,
                required: true,
            },
            ShIO {
                name: "data".to_string(),
                r#type: "object".to_string(),
                template: Value::Object(serde_json::Map::new()),
                value: None,
                required: true,
            }
        ];
        let input_values2 = vec![
            Value::Bool(false),
            Value::Object({
                let mut map = serde_json::Map::new();
                map.insert("key".to_string(), Value::String("value".to_string()));
                map
            })
        ];
        
        let result2 = engine.instantiate_io(&io_fields2, &input_values2, &types1, None);
        assert!(result2.is_ok());
        
        // Check that values were injected
        let injected_fields2 = result2.unwrap();
        assert_eq!(injected_fields2[0].value, Some(Value::Bool(false)));
        assert_eq!(injected_fields2[1].value, Some(input_values2[1].clone()));
        
        // Test case 3: Custom type injection with validation
        let types3 = Some({
            let mut map = serde_json::Map::new();
            map.insert("User".to_string(), Value::Object({
                let mut user_type = serde_json::Map::new();
                user_type.insert("name".to_string(), Value::Object({
                    let mut name_field = serde_json::Map::new();
                    name_field.insert("type".to_string(), Value::String("string".to_string()));
                    name_field.insert("required".to_string(), Value::Bool(true));
                    name_field
                }));
                user_type.insert("age".to_string(), Value::Object({
                    let mut age_field = serde_json::Map::new();
                    age_field.insert("type".to_string(), Value::String("number".to_string()));
                    age_field.insert("required".to_string(), Value::Bool(true));
                    age_field
                }));
                user_type
            }));
            map
        });
        
        let io_fields3 = vec![
            ShIO {
                name: "user".to_string(),
                r#type: "User".to_string(),
                template: Value::Object(serde_json::Map::new()),
                value: None,
                required: true,
            }
        ];
        let input_values3 = vec![Value::Object({
            let mut user_obj = serde_json::Map::new();
            user_obj.insert("name".to_string(), Value::String("Bob".to_string()));
            user_obj.insert("age".to_string(), Value::Number(30.into()));
            user_obj
        })];
        
        let result3 = engine.instantiate_io(&io_fields3, &input_values3, &types3, None);
        assert!(result3.is_ok());
        
        // Check that value was injected
        let injected_fields3 = result3.unwrap();
        assert_eq!(injected_fields3[0].value, Some(input_values3[0].clone()));
        
        // Test case 4: Mixed primitive and custom types
        let io_fields4 = vec![
            ShIO {
                name: "title".to_string(),
                r#type: "string".to_string(),
                template: Value::String("Test".to_string()),
                value: None,
                required: true,
            },
            ShIO {
                name: "user".to_string(),
                r#type: "User".to_string(),
                template: Value::Object(serde_json::Map::new()),
                value: None,
                required: true,
            }
        ];
        let input_values4 = vec![
            Value::String("Test Title".to_string()),
            Value::Object({
                let mut user_obj = serde_json::Map::new();
                user_obj.insert("name".to_string(), Value::String("Alice".to_string()));
                user_obj.insert("age".to_string(), Value::Number(25.into()));
                user_obj
            })
        ];
        
        let result4 = engine.instantiate_io(&io_fields4, &input_values4, &types3, None);
        assert!(result4.is_ok());
        
        // Check that both values were injected
        let injected_fields4 = result4.unwrap();
        assert_eq!(injected_fields4[0].value, Some(Value::String("Test Title".to_string())));
        assert_eq!(injected_fields4[1].value, Some(input_values4[1].clone()));
        
        // Test case 5: Error - invalid custom type value
        let io_fields5 = vec![
            ShIO {
                name: "user".to_string(),
                r#type: "User".to_string(),
                template: Value::Object(serde_json::Map::new()),
                value: None,
                required: true,
            }
        ];
        let input_values5 = vec![Value::Object({
            let mut user_obj = serde_json::Map::new();
            user_obj.insert("name".to_string(), Value::String("Bob".to_string()));
            // Missing required "age" field
            user_obj
        })];
        
        let result5 = engine.instantiate_io(&io_fields5, &input_values5, &types3, None);
        assert!(result5.is_err());
        assert!(result5.unwrap_err().to_string().contains("Value 0 is invalid"));
        
        // Test case 6: Error - invalid type definition
        let types6 = Some({
            let mut map = serde_json::Map::new();
            map.insert("InvalidType".to_string(), Value::String("invalid".to_string()));
            map
        });
        
        let io_fields6 = vec![
            ShIO {
                name: "invalid".to_string(),
                r#type: "InvalidType".to_string(),
                template: Value::String("test".to_string()),
                value: None,
                required: true,
            }
        ];
        let input_values6 = vec![Value::String("test".to_string())];
        
        let result6 = engine.instantiate_io(&io_fields6, &input_values6, &types6, None);
        assert!(result6.is_err());
        assert!(result6.unwrap_err().to_string().contains("Failed to compile schema for type 'InvalidType'"));
        
        // Test case 7: Empty IO fields and values
        let io_fields7 = vec![];
        let input_values7 = vec![];
        
        let result7 = engine.instantiate_io(&io_fields7, &input_values7, &types1, None);
        assert!(result7.is_ok());
        
        // Test case 8: Unknown type (should pass through)
        let io_fields8 = vec![
            ShIO {
                name: "unknown".to_string(),
                r#type: "UnknownType".to_string(),
                template: Value::String("test".to_string()),
                value: None,
                required: true,
            }
        ];
        let input_values8 = vec![Value::String("test_value".to_string())];
        
        let result8 = engine.instantiate_io(&io_fields8, &input_values8, &types3, None);
        assert!(result8.is_ok());
        
        // Check that value was injected (pass through behavior)
        let injected_fields8 = result8.unwrap();
        assert_eq!(injected_fields8[0].value, Some(Value::String("test_value".to_string())));
        
        // Test case 9: Array type injection
        let types9 = Some({
            let mut map = serde_json::Map::new();
            map.insert("UserList".to_string(), Value::Array(vec![
                Value::Object({
                    let mut user_type = serde_json::Map::new();
                    user_type.insert("name".to_string(), Value::Object({
                        let mut name_field = serde_json::Map::new();
                        name_field.insert("type".to_string(), Value::String("string".to_string()));
                        name_field.insert("required".to_string(), Value::Bool(true));
                        name_field
                    }));
                    user_type
                })
            ]));
            map
        });
        
        let io_fields9 = vec![
            ShIO {
                name: "users".to_string(),
                r#type: "UserList".to_string(),
                template: Value::Array(vec![]),
                value: None,
                required: true,
            }
        ];
        let input_values9 = vec![Value::Array(vec![
            Value::Object({
                let mut user_obj = serde_json::Map::new();
                user_obj.insert("name".to_string(), Value::String("User1".to_string()));
                user_obj
            }),
            Value::Object({
                let mut user_obj = serde_json::Map::new();
                user_obj.insert("name".to_string(), Value::String("User2".to_string()));
                user_obj
            })
        ])];
        
        let result9 = engine.instantiate_io(&io_fields9, &input_values9, &types9, None);
        assert!(result9.is_ok());
        
        // Check that value was injected
        let injected_fields9 = result9.unwrap();
        assert_eq!(injected_fields9[0].value, Some(input_values9[0].clone()));
        
        // Test case 10: Null value injection
        let io_fields10 = vec![
            ShIO {
                name: "description".to_string(),
                r#type: "string".to_string(),
                template: Value::String("".to_string()),
                value: None,
                required: true,
            }
        ];
        let input_values10 = vec![Value::Null];
        
        let result10 = engine.instantiate_io(&io_fields10, &input_values10, &types1, None);
        assert!(result10.is_ok());
        
        // Check that null value was injected
        let injected_fields10 = result10.unwrap();
        assert_eq!(injected_fields10[0].value, Some(Value::Null));
    }

    #[tokio::test]
    async fn test_execute_action_get_weather_by_location_name() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing action with the same inputs as test_build_action_tree
        let action_ref = "starthubhq/get-weather-by-location-name:0.0.1";
        let inputs = vec![
            json!({
                "location_name": "Rome",
                "open_weather_api_key": "f13e712db9557544db878888528a5e29"
            })
        ];
        
        let result = engine.execute_action(action_ref, inputs).await;
        
        // println!("result: {:#?}", result);
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid action_ref and inputs");
        
        let outputs = result.unwrap();
        
        // The function returns an array of output values directly
        assert!(outputs.is_array());
        let outputs_array = outputs.as_array().unwrap();
        assert_eq!(outputs_array.len(), 1);
        
        let output = &outputs_array[0];
        assert!(output.is_object());
        let output_obj = output.as_object().unwrap();
        
        // Verify the output has the expected structure with location_name and weather
        assert!(output_obj.contains_key("location_name"));
        assert!(output_obj.contains_key("weather"));
        assert_eq!(output_obj["location_name"], "Rome");
        // Weather description can vary, just check it's a non-empty string
        assert!(output_obj["weather"].is_string());
        assert!(!output_obj["weather"].as_str().unwrap().is_empty());
    }

    // #[tokio::test]
    // async fn test_execute_action_create_do_project() {
    //     dotenv::dotenv().ok();

    //     // Create a mock ExecutionEngine
    //     let mut engine = ExecutionEngine::new();
        
    //     // Test executing action with the same inputs as test_build_action_tree
    //     let action_ref = "starthubhq/do-create-project:0.0.1";
        
    //     // Read test parameters from environment variables with defaults
    //     let api_token = std::env::var("DO_API_TOKEN")
    //         .unwrap_or_else(|_| "".to_string());
    //     let name = std::env::var("DO_PROJECT_NAME")
    //         .unwrap_or_else(|_| "".to_string());
    //     let description = std::env::var("DO_PROJECT_DESCRIPTION")
    //         .unwrap_or_else(|_| "".to_string());
    //     let purpose = std::env::var("DO_PROJECT_PURPOSE")
    //         .unwrap_or_else(|_| "".to_string());
    //     let environment = std::env::var("DO_PROJECT_ENVIRONMENT")
    //         .unwrap_or_else(|_| "".to_string());
        
    //     let inputs = vec![
    //         json!({
    //             "api_token": api_token,
    //             "name": name,
    //             "description": description,
    //             "purpose": purpose,
    //             "environment": environment
    //         })
    //     ];
        
    //     println!("inputs: {:#?}", inputs);
    //     let result = engine.execute_action(action_ref, inputs).await;
        
    //     println!("result: {:#?}", result);
    //     // The test should succeed
    //     assert!(result.is_ok(), "execute_action should succeed for valid action_ref and inputs");
    // }

    // #[tokio::test]
    // async fn test_execute_action_create_do_droplet() {
    //     dotenv::dotenv().ok();

    //     // Create a mock ExecutionEngine
    //     let mut engine = ExecutionEngine::new();
        
    //     // Test executing action for droplet creation
    //     let action_ref = "starthubhq/do-create-droplet:0.0.1";
        
    //     // Read test parameters from environment variables with defaults
    //     let api_token = std::env::var("DO_API_TOKEN")
    //         .unwrap_or_else(|_| "".to_string());
    //     let name = std::env::var("DO_DROPLET_NAME")
    //         .unwrap_or_else(|_| "test-droplet".to_string());
    //     let region = std::env::var("DO_DROPLET_REGION")
    //         .unwrap_or_else(|_| "nyc1".to_string());
    //     let size = std::env::var("DO_DROPLET_SIZE")
    //         .unwrap_or_else(|_| "s-1vcpu-1gb".to_string());
    //     let image = std::env::var("DO_DROPLET_IMAGE")
    //         .unwrap_or_else(|_| "ubuntu-20-04-x64".to_string());
    //     let ssh_keys = std::env::var("DO_DROPLET_SSH_KEYS")
    //         .unwrap_or_else(|_| "".to_string());
    //     let backups = std::env::var("DO_DROPLET_BACKUPS")
    //         .unwrap_or_else(|_| "false".to_string());
    //     let ipv6 = std::env::var("DO_DROPLET_IPV6")
    //         .unwrap_or_else(|_| "false".to_string());
    //     let monitoring = std::env::var("DO_DROPLET_MONITORING")
    //         .unwrap_or_else(|_| "false".to_string());
    //     let tags = std::env::var("DO_DROPLET_TAGS")
    //         .unwrap_or_else(|_| "".to_string());
    //     let user_data = std::env::var("DO_DROPLET_USER_DATA")
    //         .unwrap_or_else(|_| "".to_string());
        
    //     // Parse boolean values
    //     let backups_bool = backups.parse::<bool>().unwrap_or(false);
    //     let ipv6_bool = ipv6.parse::<bool>().unwrap_or(false);
    //     let monitoring_bool = monitoring.parse::<bool>().unwrap_or(false);
        
    //     // Parse array values
    //     let _ssh_keys_array: Vec<String> = if ssh_keys.is_empty() {
    //         vec![]
    //     } else {
    //         ssh_keys.split(',').map(|s| s.trim().to_string()).collect()
    //     };
        
    //     let tags_array: Vec<String> = if tags.is_empty() {
    //         vec![]
    //     } else {
    //         tags.split(',').map(|s| s.trim().to_string()).collect()
    //     };
        
    //     let inputs = vec![
    //         json!({
    //             "api_token": api_token,
    //             "name": name,
    //             "region": region,
    //             "size": size,
    //             "image": image,
    //             "backups": backups_bool,
    //             "ipv6": ipv6_bool,
    //             "monitoring": monitoring_bool,
    //             "tags": tags_array,
    //             "user_data": user_data
    //         })
    //     ];
        
    //     // println!("inputs: {:#?}", inputs);
    //     let result = engine.execute_action(action_ref, inputs).await;
    //     println!("result: {:#?}", result);
    //     // The test should succeed
    //     assert!(result.is_ok(), "execute_action should succeed for valid action_ref and inputs");
    // }

    // #[tokio::test]
    // async fn test_execute_action_create_do_ssh_key_from_file() {
    //     dotenv::dotenv().ok();

    //     // Create a mock ExecutionEngine
    //     let mut engine = ExecutionEngine::new();
        
    //     // Test executing action for SSH key creation from file
    //     let action_ref = "starthubhq/do-create-ssh-key:0.0.1";
        
    //     // Read test parameters from environment variables with defaults
    //     let api_token = std::env::var("DO_API_TOKEN")
    //         .unwrap_or_else(|_| "".to_string());
    //     let name = std::env::var("DO_SSH_KEY_NAME")
    //         .unwrap_or_else(|_| "test-ssh-key-from-file".to_string());
    //     let ssh_key_file_path = std::env::var("DO_SSH_KEY_FILE_PATH")
    //         .unwrap_or_else(|_| "/tmp/test_ssh_key.pub".to_string());
        
    //     // Create a temporary SSH key file for testing if it doesn't exist
    //     if !std::path::Path::new(&ssh_key_file_path).exists() {
    //         let test_public_key = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQC7vbqajDhA test@example.com";
    //         if let Err(e) = std::fs::write(&ssh_key_file_path, test_public_key) {
    //             println!("Warning: Could not create test SSH key file at {}: {}", ssh_key_file_path, e);
    //         }
    //     }
        
    //     let inputs = vec![
    //         json!({
    //             "api_token": api_token,
    //             "name": name,
    //             "ssh_key_file_path": ssh_key_file_path
    //         })
    //     ];
        
    //     println!("inputs: {:#?}", inputs);
    //     let result = engine.execute_action(action_ref, inputs).await;
        
    //     println!("result: {:#?}", result);
    //     // The test should succeed
    //     assert!(result.is_ok(), "execute_action should succeed for valid action_ref and inputs with file path");
    // }

    // #[tokio::test]
    // async fn test_execute_action_create_do_droplet_sync() {
    //     dotenv::dotenv().ok();

    //     // Create a mock ExecutionEngine
    //     let mut engine = ExecutionEngine::new();
        
    //     // Test executing action for droplet creation with sync
    //     let action_ref = "starthubhq/do-create-droplet-sync:0.0.1";
        
    //     // Read test parameters from environment variables with defaults
    //     let api_token = std::env::var("DO_API_TOKEN")
    //         .unwrap_or_else(|_| "".to_string());
    //     let name = std::env::var("DO_DROPLET_NAME")
    //         .unwrap_or_else(|_| "test-droplet-sync".to_string());
    //     let region = std::env::var("DO_DROPLET_REGION")
    //         .unwrap_or_else(|_| "nyc1".to_string());
    //     let size = std::env::var("DO_DROPLET_SIZE")
    //         .unwrap_or_else(|_| "s-1vcpu-1gb".to_string());
    //     let image = std::env::var("DO_DROPLET_IMAGE")
    //         .unwrap_or_else(|_| "ubuntu-20-04-x64".to_string());
    //     let backups = std::env::var("DO_DROPLET_BACKUPS")
    //         .unwrap_or_else(|_| "false".to_string());
    //     let ipv6 = std::env::var("DO_DROPLET_IPV6")
    //         .unwrap_or_else(|_| "false".to_string());
    //     let monitoring = std::env::var("DO_DROPLET_MONITORING")
    //         .unwrap_or_else(|_| "false".to_string());
    //     let tags = std::env::var("DO_DROPLET_TAGS")
    //         .unwrap_or_else(|_| "".to_string());
    //     let user_data = std::env::var("DO_DROPLET_USER_DATA")
    //         .unwrap_or_else(|_| "".to_string());
        
    //     // Parse boolean values
    //     let backups_bool = backups.parse::<bool>().unwrap_or(false);
    //     let ipv6_bool = ipv6.parse::<bool>().unwrap_or(false);
    //     let monitoring_bool = monitoring.parse::<bool>().unwrap_or(false);
        
    //     // Parse array values
    //     let tags_array: Vec<String> = if tags.is_empty() {
    //         vec![]
    //     } else {
    //         tags.split(',').map(|s| s.trim().to_string()).collect()
    //     };
        
    //     let inputs = vec![
    //         json!({
    //             "api_token": api_token,
    //             "name": name,
    //             "region": region,
    //             "size": size,
    //             "image": image,
    //             "backups": backups_bool,
    //             "ipv6": ipv6_bool,
    //             "monitoring": monitoring_bool,
    //             "tags": tags_array,
    //             "user_data": user_data
    //         })
    //     ];
        
    //     // println!("inputs: {:#?}", inputs);
    //     let result = engine.execute_action(action_ref, inputs).await;
    //     println!("result: {:#?}", result);
    //     // The test should succeed
    //     // assert!(result.is_ok(), "execute_action should succeed for valid action_ref and inputs");
    // }

    // #[tokio::test]
    // async fn test_execute_action_get_do_droplet() {
    //     dotenv::dotenv().ok();

    //     // Create a mock ExecutionEngine
    //     let mut engine = ExecutionEngine::new();
        
    //     // Test executing action for droplet retrieval
    //     let action_ref = "starthubhq/do-get-droplet:0.0.1";
        
    //     // Read test parameters from environment variables with defaults
    //     let api_token = std::env::var("DO_API_TOKEN")
    //         .unwrap_or_else(|_| "".to_string());
    //     let droplet_id = std::env::var("DO_DROPLET_ID")
    //         .unwrap_or_else(|_| "123456789".to_string());
        
    //     let inputs = vec![
    //         json!({
    //             "api_token": api_token,
    //             "droplet_id": droplet_id
    //         })
    //     ];
        
    //     println!("Testing do-get-droplet with inputs: {:#?}", inputs);
    //     let result = engine.execute_action(action_ref, inputs).await;
        
    //     println!("do-get-droplet test result: {:#?}", result);
    //     // The test should succeed
    //     assert!(result.is_ok(), "execute_action should succeed for valid do-get-droplet action_ref and inputs");
        
    //     let action_tree = result.unwrap();
        
    //     // Verify the action structure
    //     assert_eq!(action_tree["name"], "do-get-droplet");
    //     assert_eq!(action_tree["kind"], "composition");
    //     assert_eq!(action_tree["uses"], action_ref);
        
    //     // Verify inputs
    //     assert!(action_tree["inputs"].is_array());
    //     let inputs_array = action_tree["inputs"].as_array().unwrap();
    //     assert_eq!(inputs_array.len(), 1);
    //     let input = &inputs_array[0];
    //     assert_eq!(input["name"], "droplet_config");
    //     assert_eq!(input["type"], "DigitalOceanDropletGetConfig");
        
    //     // Verify outputs
    //     assert!(action_tree["outputs"].is_array());
    //     let outputs_array = action_tree["outputs"].as_array().unwrap();
    //     assert_eq!(outputs_array.len(), 1);
    //     let output = &outputs_array[0];
    //     assert_eq!(output["name"], "droplet");
    //     assert_eq!(output["type"], "DigitalOceanDroplet");
        
    //     // Execution order is now determined dynamically at runtime
        
    //     // Verify types are present
    //     assert!(action_tree["types"].is_object());
    //     let types = action_tree["types"].as_object().unwrap();
    //     assert!(types.contains_key("DigitalOceanDropletGetConfig"));
    //     assert!(types.contains_key("DigitalOceanDroplet"));
        
    //     // Verify permissions
    //     assert!(action_tree["permissions"].is_object());
    //     let permissions = action_tree["permissions"].as_object().unwrap();
    //     assert!(permissions.contains_key("net"));
    //     let net_permissions = permissions["net"].as_array().unwrap();
    //     assert!(net_permissions.contains(&json!("http")));
    //     assert!(net_permissions.contains(&json!("https")));
    // }

    // #[tokio::test]
    // async fn test_execute_action_std_read_file() {
    //     // Create a mock ExecutionEngine
    //     let mut engine = ExecutionEngine::new();
        
    //     // Test executing the std/read-file action
    //     let action_ref = "std/read-file:0.0.1";
        
    //     // Test with file path parameter
    //     let inputs = vec![
    //         json!("/Users/tommaso/Desktop/test.txt")
    //     ];
        
    //     println!("Testing std/read-file with inputs: {:#?}", inputs);
    //     let result = engine.execute_action(action_ref, inputs).await;
        
    //     println!("std/read-file test result: {:#?}", result);
    //     // The test should succeed
    //     assert!(result.is_ok(), "execute_action should succeed for valid std/read-file action_ref and inputs");
        
    //     let action_tree = result.unwrap();
        
    //     // Verify the action structure
    //     assert_eq!(action_tree["name"], "read-file");
    //     assert_eq!(action_tree["kind"], "wasm");
    //     assert_eq!(action_tree["uses"], action_ref);
    // }

    #[tokio::test]
    async fn test_execute_action_sleep() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing the sleep action directly
        let action_ref = "std/sleep:0.0.1";
        
        // Test with three inputs: seconds, next_step, and depends_on
        let inputs = vec![
            json!(5),  // seconds
            json!("next_step_id"),  // next_step
            json!("dependency_step_id")  // depends_on
        ];
        
        let result = engine.execute_action(action_ref, inputs).await;
        
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid sleep action_ref and inputs");
        
        let outputs = result.unwrap();
        
        // Verify the outputs contain the next step ID
        assert!(outputs.is_array(), "outputs should be an array");
        
        let outputs_array = outputs.as_array().unwrap();
        assert!(!outputs_array.is_empty(), "outputs array should not be empty");
        
        // Check that the first output contains the next step ID
        let first_output = &outputs_array[0];
        assert_eq!(first_output, "next_step_id");
    }

    #[tokio::test]
    async fn test_execute_action_cast() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing the cast action directly
        let action_ref = "std/cast:0.0.1";
        
        // Test with three inputs: value, input_type, and output_type
        let inputs = vec![
            json!("123"),  // value (string)
            json!("string"),  // input_type
            json!("number")   // output_type
        ];
        
        let result = engine.execute_action(action_ref, inputs).await;
        
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid cast action_ref and inputs");
        
        let outputs = result.unwrap();
        
        // Should have one output with the casted value
        assert!(outputs.is_array(), "outputs should be an array");
        let outputs_array = outputs.as_array().unwrap();
        assert_eq!(outputs_array.len(), 1);
        assert_eq!(outputs_array[0], json!(123.0)); // Should be cast to number
    }

    #[tokio::test]
    async fn test_execute_action_if() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing the if action directly
        let action_ref = "std/if:0.0.1";
        
        // Test with five inputs: a, operator, b, then, else
        let inputs = vec![
            json!(15),  // a (value to compare)
            json!(">"),   // operator
            json!(10),  // b (comparison value)
            json!("then_step"),  // then (step to execute if true)
            json!("else_step")   // else (step to execute if false)
        ];
        
        let result = engine.execute_action(action_ref, inputs).await;
        
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid if action_ref and inputs");
        
        let outputs = result.unwrap();
        println!("outputs: {:#?}", outputs);
        // Should have one output with the step ID to execute
        assert!(outputs.is_array(), "outputs should be an array");
        let outputs_array = outputs.as_array().unwrap();
        assert_eq!(outputs_array.len(), 1);
        assert_eq!(outputs_array[0], json!("then_step")); // Should return "then_step" since 15 > 10
    }

    #[tokio::test]
    async fn test_execute_action_if_else() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing the if action directly with a < b condition
        let action_ref = "std/if:0.0.1";
        
        // Test with five inputs: a, operator, b, then, else
        let inputs = vec![
            json!(5),   // a (value to compare)
            json!("<"), // operator
            json!(10),  // b (comparison value)
            json!("then_step"),  // then (step to execute if true)
            json!("else_step")   // else (step to execute if false)
        ];
        
        let result = engine.execute_action(action_ref, inputs).await;
        
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid if action_ref and inputs");
        
        let outputs = result.unwrap();
        println!("outputs: {:#?}", outputs);
        
        // Should have one output with the step ID to execute
        assert!(outputs.is_array(), "outputs should be an array");
        let outputs_array = outputs.as_array().unwrap();
        assert_eq!(outputs_array.len(), 1);
        assert_eq!(outputs_array[0], json!("then_step")); // Should return "then_step" since 5 < 10
    }

    #[tokio::test]
    async fn test_execute_action_poll_weather() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing the poll weather action
        let action_ref = "starthubhq/poll-weather-by-location-name:0.0.1";
        
        // Test with weather config input
        let inputs = vec![
            json!({
                "location_name": "rome",
                "open_weather_api_key": "f13e712db9557544db878888528a5e29"
            })
        ];
        
        let result = engine.execute_action(action_ref, inputs).await;
        
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid poll weather action_ref and inputs");
        
        let action_tree = result.unwrap();
        
        // Verify the action structure
        assert_eq!(action_tree["kind"], "composition");
        assert_eq!(action_tree["uses"], action_ref);
        
        // Verify the composition has steps
        let steps = &action_tree["steps"];
        assert!(steps.is_object(), "steps should be an object");
        
        let steps_obj = steps.as_object().unwrap();
        assert!(steps_obj.contains_key("get_weather"), "should contain get_weather step");
        assert!(steps_obj.contains_key("sleep"), "should contain sleep step");
        
        // Verify get_weather step uses the correct action
        let get_weather_step = &steps_obj["get_weather"];
        assert_eq!(get_weather_step["uses"], "starthubhq/get-weather-by-location-name:0.0.1");
        
        // Verify sleep step uses the correct action
        let sleep_step = &steps_obj["sleep"];
        assert_eq!(sleep_step["uses"], "starthubhq/sleep:0.0.1");
        
        // Verify sleep step has flow control inputs
        let sleep_inputs = &sleep_step["inputs"];
        assert!(sleep_inputs.is_array(), "sleep inputs should be an array");
        
        let sleep_inputs_array = sleep_inputs.as_array().unwrap();
        assert_eq!(sleep_inputs_array.len(), 2, "sleep should have 2 inputs");
        
        // Check seconds input
        let seconds_input = &sleep_inputs_array[0];
        assert_eq!(seconds_input["name"], "seconds");
        assert_eq!(seconds_input["value"], 5);
        
        // Check next_step input
        let next_step_input = &sleep_inputs_array[1];
        assert_eq!(next_step_input["name"], "next_step");
        assert_eq!(next_step_input["value"], "get_weather");
    }

    // #[tokio::test]
    // async fn test_execute_action_base64_to_text() {
    //     // Create a mock ExecutionEngine
    //     let mut engine = ExecutionEngine::new();
        
    //     // Test executing the base64-to-text action directly
    //     let action_ref = "starthubhq/base64-to-text:0.0.1";
        
    //     // Test with base64 encoded "Hello World" and ignored value
    //     let inputs = vec![
    //         json!("SGVsbG8gV29ybGQ="),  // base64 encoded "Hello World"
    //         json!("ignored_value")  // second input that will be ignored
    //     ];
        
    //     println!("Testing base64-to-text action with inputs: {:#?}", inputs);
    //     let result = engine.execute_action(action_ref, inputs).await;
        
    //     println!("Base64-to-text test result: {:#?}", result);
    //     // The test should succeed
    //     assert!(result.is_ok(), "execute_action should succeed for valid base64-to-text action_ref and inputs");
        
    //     let action_tree = result.unwrap();
        
    //     // Verify the action structure
    //     assert_eq!(action_tree["name"], "base64-to-text");
    //     assert_eq!(action_tree["kind"], "wasm");
    //     assert_eq!(action_tree["uses"], action_ref);
        
    //     // Verify that the action has the expected inputs and outputs
    //     assert!(action_tree["inputs"].is_array());
    //     let inputs_array = action_tree["inputs"].as_array().unwrap();
        
    //     // Check first input (base64_string)
    //     let first_input = &inputs_array[0];
    //     assert_eq!(first_input["name"], "base64_string");
    //     assert_eq!(first_input["type"], "string");
    //     assert_eq!(first_input["required"], true);
        
    //     // Verify outputs
    //     assert!(action_tree["outputs"].is_array());
    //     let outputs_array = action_tree["outputs"].as_array().unwrap();
    //     assert_eq!(outputs_array.len(), 1);
        
    //     let output = &outputs_array[0];
    //     assert_eq!(output["name"], "text");
    //     assert_eq!(output["type"], "string");
    // }

    // #[tokio::test]
    // async fn test_execute_action_file_to_string() {
    //     // Create a mock ExecutionEngine
    //     let mut engine = ExecutionEngine::new();
        
    //     // Test executing the file-to-string composition
    //     let action_ref = "starthubhq/file-to-string:0.0.1";
        
    //     // Test with file path parameter
    //     let inputs = vec![
    //         json!({
    //             "file_path": "/Users/tommaso/Desktop/test.txt"
    //         })
    //     ];
        
    //     println!("Testing file-to-string composition with inputs: {:#?}", inputs);
    //     let result = engine.execute_action(action_ref, inputs).await;
        
    //     println!("File-to-string test result: {:#?}", result);
    //     // The test should succeed
    //     assert!(result.is_ok(), "execute_action should succeed for valid file-to-string action_ref and inputs");
        
    //     let action_tree = result.unwrap();
        
    //     // Verify the action structure
    //     assert_eq!(action_tree["name"], "file-to-string");
    //     assert_eq!(action_tree["kind"], "composition");
    //     assert_eq!(action_tree["uses"], action_ref);
        
    //     // Verify inputs
    //     assert!(action_tree["inputs"].is_array());
    //     let inputs_array = action_tree["inputs"].as_array().unwrap();
    //     assert_eq!(inputs_array.len(), 1);
    //     let input = &inputs_array[0];
    //     assert_eq!(input["name"], "file_config");
    //     assert_eq!(input["type"], "FileConfig");
        
    //     // Verify outputs
    //     assert!(action_tree["outputs"].is_array());
    //     let outputs_array = action_tree["outputs"].as_array().unwrap();
    //     assert_eq!(outputs_array.len(), 1);
    //     let output = &outputs_array[0];
    //     assert_eq!(output["name"], "content");
    //     assert_eq!(output["type"], "string");
        
    //     // Execution order is now determined dynamically at runtime
        
    //     // Verify types are present
    //     assert!(action_tree["types"].is_object());
    //     let types = action_tree["types"].as_object().unwrap();
    //     assert!(types.contains_key("FileConfig"));
        
    //     // Verify permissions
    //     assert!(action_tree["permissions"].is_object());
    //     let permissions = action_tree["permissions"].as_object().unwrap();
    //     assert!(permissions.contains_key("fs"));
    //     let fs_permissions = permissions["fs"].as_array().unwrap();
    //     assert!(fs_permissions.contains(&json!("read")));
    // }
    
    // #[test]
    // fn test_execution_engine_new() {
    //     // Test creating a new ExecutionEngine
    //     let engine = ExecutionEngine::new();
        
    //     // Verify the cache directory is set correctly
    //     let expected_cache_dir = dirs::cache_dir()
    //         .unwrap_or(std::env::temp_dir())
    //         .join("starthub/oci");
        
    //     assert_eq!(engine.cache_dir, expected_cache_dir);
    // }

    // #[tokio::test]
    // async fn test_build_action_tree() {
    //     // Create a mock ExecutionEngine
    //     let engine = ExecutionEngine::new();
        
    //     // Test building action tree for the coordinates action
    //     let action_ref = "starthubhq/get-weather-by-location-name:0.0.1";
    //     let result = engine.build_action_tree(action_ref, None).await;
        
    //     // The test should succeed
    //     assert!(result.is_ok(), "build_action_tree should succeed for valid action_ref");
        
    //     let action_tree = result.unwrap();
        
    //     // Verify the root action structure
    //     assert_eq!(action_tree.name, "get-weather-by-location-name");
    //     assert_eq!(action_tree.kind, "composition");
    //     assert_eq!(action_tree.uses, action_ref);
    //     assert!(action_tree.parent_action.is_none());
        
    //     // Verify inputs
    //     assert_eq!(action_tree.inputs.len(), 1);
    //     let input = &action_tree.inputs[0];
    //     assert_eq!(input.name, "weather_config");
    //     assert_eq!(input.r#type, "WeatherConfig");
        
    //     // Verify outputs
    //     assert_eq!(action_tree.outputs.len(), 1);
    //     let output = &action_tree.outputs[0];
    //     assert_eq!(output.name, "response");
    //     assert_eq!(output.r#type, "CustomWeatherResponse");
        
    //     // Execution order is now determined dynamically at runtime
        
    //     // Verify types are present
    //     assert!(action_tree.types.is_some());
    //     let types = action_tree.types.as_ref().unwrap();
    //     assert!(types.contains_key("WeatherConfig"));
    //     assert!(types.contains_key("CustomWeatherResponse"));
    // }

    




    // #[tokio::test]
    // async fn test_fetch_manifest() {
    //     // Create a mock ExecutionEngine
    //     let engine = ExecutionEngine::new();
        
    //     // Test fetching a real manifest from the starthub API
    //     let action_ref = "starthubhq/get-weather-by-location-name:0.0.1";
    //     let result = engine.fetch_manifest(action_ref).await;
        
    //     // The test should succeed and return a valid manifest
    //     assert!(result.is_ok(), "fetch_manifest should succeed for valid action_ref");
        
    //     let manifest = result.unwrap();
        
    //     // Verify the manifest has the expected structure
    //     assert_eq!(manifest.name, "get-weather-by-location-name");
    //     assert_eq!(manifest.version, "0.0.1");
    //     assert_eq!(manifest.kind, Some(ShKind::Composition));
        
    //     // Verify it has inputs
    //     assert!(manifest.inputs.is_array());
    //     assert!(manifest.inputs.as_array().unwrap().len() > 0);
        
    //     // Verify it has outputs
    //     assert!(manifest.outputs.is_array());
    //     assert!(manifest.outputs.as_array().unwrap().len() > 0);
    // }

    // #[tokio::test]
    // async fn test_fetch_manifest_invalid_ref() {
    //     // Create a mock ExecutionEngine
    //     let engine = ExecutionEngine::new();
        
    //     // Test fetching a non-existent manifest
    //     let action_ref = "starthubhq/non-existent-action:1.0.0";
    //     let result = engine.fetch_manifest(action_ref).await;
        
    //     // The test should fail for invalid action_ref
    //     assert!(result.is_err(), "fetch_manifest should fail for invalid action_ref");
    // }

    // #[test]
    // fn test_resolve_inputs() {
    //     // Create a mock ExecutionEngine
    //     let engine = ExecutionEngine::new();
        
    //     // Create mock types for validation
    //     let mut types = HashMap::new();
    //     types.insert("WeatherConfig".to_string(), json!({
    //         "location_name": {
    //             "type": "string",
    //             "description": "The name of the location to get weather for",
    //             "required": true
    //         },
    //         "open_weather_api_key": {
    //             "type": "string", 
    //             "description": "OpenWeatherMap API key",
    //             "required": true
    //         }
    //     }));
        
    //     // Create mock action state with inputs and types
    //     let action_state = ShAction {
    //         id: "test-action".to_string(),
    //         name: "test-action".to_string(),
    //         kind: "composition".to_string(),
    //         uses: "test:action".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "weather_config".to_string(),
    //                 r#type: "WeatherConfig".to_string(),
    //                 template: json!("{{inputs[0].location_name}}"),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         execution_order: vec![],
    //         types: Some(types.clone().into_iter().collect()),
    //     };
        
    //     // Test case 1: Valid inputs that match the schema
    //     let valid_inputs = vec![
    //         json!({
    //             "location_name": "Rome",
    //             "open_weather_api_key": "f13e712db9557544db878888528a5e29"
    //         })
    //     ];
        
    //     let result = engine.resolve_child_inputs(
    //         &action_state.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()), 
    //         &action_state.inputs,
    //         &valid_inputs,
    //         &HashMap::new());
    //     assert!(result.is_ok(), "resolve_inputs should succeed for valid inputs");
        
    //     let resolved_inputs = result.unwrap();
    //     assert_eq!(resolved_inputs.len(), 1);
    //     // Since resolve_template_string is not implemented yet, templates are returned as-is
    //     // The resolved value will be the template string, not the actual value
    //     assert_eq!(resolved_inputs[0], json!("{{inputs[0].location_name}}"));
        
    //     // Test case 2: Invalid inputs that don't match the schema (missing required field)
    //     let invalid_inputs = vec![
    //         json!({
    //             "location_name": "Rome"
    //             // Missing open_weather_api_key
    //         })
    //     ];
        
    //     let result = engine.resolve_child_inputs(&action_state.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()), 
    //     &action_state.inputs,
    //     &invalid_inputs,
    //     &HashMap::new());
    //     assert!(result.is_err(), "resolve_inputs should fail for invalid inputs");
        
    //     // Test case 3: No inputs provided
    //     let empty_inputs = vec![];
    //     let result = engine.resolve_child_inputs(&action_state.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()), 
    //     &action_state.inputs,
    //     &empty_inputs,
    //     &HashMap::new());
    //     assert!(result.is_err(), "resolve_inputs should fail when no inputs provided");
        
    //     // Test case 4: Action state with no types defined
    //     let action_state_no_types = ShAction {
    //         id: "test-action".to_string(),
    //         name: "test-action".to_string(),
    //         kind: "composition".to_string(),
    //         uses: "test:action".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "weather_config".to_string(),
    //                 r#type: "WeatherConfig".to_string(),
    //                 template: json!({}),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         execution_order: vec![],
    //         types: None, // No types defined
    //     };
        
    //     let result = engine.resolve_child_inputs(&action_state_no_types.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()), 
    //     &action_state_no_types.inputs,
    //     &valid_inputs,
    //     &HashMap::new());
    //     assert!(result.is_err(), "resolve_inputs should fail when no types are defined");
    // }

    // #[tokio::test]
    // async fn test_run_action_tree_wasm_early_return() {
    //     // Test that WASM actions return early without processing
    //     let mut engine = ExecutionEngine::new();
        
    //     let mut wasm_action = ShAction {
    //         id: "test-wasm".to_string(),
    //         name: "test-wasm".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test:wasm".to_string(),
    //         inputs: vec![],
    //         outputs: vec![],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         flow_control: false,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     let inputs = vec![json!({"test": "data"})];
    //     let result = engine.run_action_tree(&mut wasm_action, &inputs, &HashMap::new()).await;
        
    //     // Should succeed and return early without processing
    //     assert!(result.is_ok(), "run_action_tree should succeed for wasm action");
    // }

    // #[test]
    // fn test_resolve_template() {
    //     let engine = ExecutionEngine::new();
        
    //     // Test case 1: String template
    //     let template_string = json!("Hello {{inputs[0].name}}");
    //     let parent_inputs = vec![json!({"name": "World"})];
    //     let executed_steps = HashMap::new();
        
    //     let result = engine.interpolate(&template_string, &parent_inputs, &executed_steps);
    //     assert!(result.is_ok(), "resolve_template should succeed for string template");
    //     let resolved = result.unwrap();
    //     println!("Resolved: {:#?}", resolved);
    //     assert_eq!(resolved, json!("Hello {{inputs[0].name}}")); // Currently returns as-is since resolve_template_string is not implemented
        
    //     // Test case 2: Object template
    //     let template_object = json!({
    //         "name": "{{inputs[0].name}}",
    //         "age": 25,
    //         "nested": {
    //             "city": "{{inputs[0].city}}"
    //         }
    //     });
    //     let parent_inputs_obj = vec![json!({"name": "Alice", "city": "New York"})];
        
    //     let result = engine.interpolate(&template_object, &parent_inputs_obj, &executed_steps);
    //     assert!(result.is_ok(), "resolve_template should succeed for object template");
    //     let resolved = result.unwrap();
    //     assert_eq!(resolved["name"], json!("{{inputs[0].name}}")); // Currently returns as-is
    //     assert_eq!(resolved["age"], json!(25)); // Non-template values preserved
    //     assert_eq!(resolved["nested"]["city"], json!("{{inputs[0].city}}")); // Currently returns as-is
        
    //     // Test case 3: Array template
    //     let template_array = json!([
    //         "{{inputs[0].item1}}",
    //         "{{inputs[0].item2}}",
    //         "static_value"
    //     ]);
    //     let parent_inputs_arr = vec![json!({"item1": "value1", "item2": "value2"})];
        
    //     let result = engine.interpolate(&template_array, &parent_inputs_arr, &executed_steps);
    //     assert!(result.is_ok(), "resolve_template should succeed for array template");
    //     let resolved = result.unwrap();
    //     assert!(resolved.is_array());
    //     let resolved_array = resolved.as_array().unwrap();
    //     assert_eq!(resolved_array.len(), 3);
    //     assert_eq!(resolved_array[0], json!("{{inputs[0].item1}}")); // Currently returns as-is
    //     assert_eq!(resolved_array[1], json!("{{inputs[0].item2}}")); // Currently returns as-is
    //     assert_eq!(resolved_array[2], json!("static_value")); // Non-template values preserved
        
    //     // Test case 4: Non-string/non-object/non-array template (should be returned as-is)
    //     let template_number = json!(42);
    //     let result = engine.interpolate(&template_number, &parent_inputs, &executed_steps);
    //     assert!(result.is_ok(), "resolve_template should succeed for number template");
    //     let resolved = result.unwrap();
    //     assert_eq!(resolved, json!(42));
        
    //     // Test case 5: Null template
    //     let template_null = json!(null);
    //     let result = engine.interpolate(&template_null, &parent_inputs, &executed_steps);
    //     assert!(result.is_ok(), "resolve_template should succeed for null template");
    //     let resolved = result.unwrap();
    //     assert_eq!(resolved, json!(null));
        
    //     // Test case 6: Boolean template
    //     let template_bool = json!(true);
    //     let result = engine.interpolate(&template_bool, &parent_inputs, &executed_steps);
    //     assert!(result.is_ok(), "resolve_template should succeed for boolean template");
    //     let resolved = result.unwrap();
    //     assert_eq!(resolved, json!(true));
    // }

    // #[test]
    // fn test_instantiate_primitive_types() {
    //     let engine = ExecutionEngine::new();
        
    //     // Test case 1: String type
    //     let io_definitions = vec![
    //         ShIO {
    //             name: "test_string".to_string(),
    //             r#type: "string".to_string(),
    //             template: json!("test"),
    //             value: None,
    //             required: true,
    //         }
    //     ];
    //     let io_values = vec![json!("hello world")];
    //     let types = None;
        
    //     let result = engine.cast(&types, &io_definitions, &io_values);
    //     assert!(result.is_ok(), "instantiate should succeed for string type");
    //     let instantiated = result.unwrap();
    //     assert_eq!(instantiated.len(), 1);
    //     assert_eq!(instantiated[0], json!("hello world"));
        
    //     // Test case 2: Boolean type
    //     let io_definitions = vec![
    //         ShIO {
    //             name: "test_bool".to_string(),
    //             r#type: "bool".to_string(),
    //             template: json!(true),
    //             value: None,
    //             required: true,
    //         }
    //     ];
    //     let io_values = vec![json!(false)];
        
    //     let result = engine.cast(&types, &io_definitions, &io_values);
    //     assert!(result.is_ok(), "instantiate should succeed for bool type");
    //     let instantiated = result.unwrap();
    //     assert_eq!(instantiated.len(), 1);
    //     assert_eq!(instantiated[0], json!(false));
        
    //     // Test case 3: Number type
    //     let io_definitions = vec![
    //         ShIO {
    //             name: "test_number".to_string(),
    //             r#type: "number".to_string(),
    //             template: json!(42),
    //             value: None,
    //             required: true,
    //         }
    //     ];
    //     let io_values = vec![json!(3.14)];
        
    //     let result = engine.cast(&types, &io_definitions, &io_values);
    //     assert!(result.is_ok(), "instantiate should succeed for number type");
    //     let instantiated = result.unwrap();
    //     assert_eq!(instantiated.len(), 1);
    //     assert_eq!(instantiated[0], json!(3.14));
        
    //     // Test case 4: Object type
    //     let io_definitions = vec![
    //         ShIO {
    //             name: "test_object".to_string(),
    //             r#type: "object".to_string(),
    //             template: json!({}),
    //             value: None,
    //             required: true,
    //         }
    //     ];
    //     let io_values = vec![json!({"key": "value", "nested": {"inner": 123}})];
        
    //     let result = engine.cast(&types, &io_definitions, &io_values);
    //     assert!(result.is_ok(), "instantiate should succeed for object type");
    //     let instantiated = result.unwrap();
    //     assert_eq!(instantiated.len(), 1);
    //     assert_eq!(instantiated[0], json!({"key": "value", "nested": {"inner": 123}}));
    // }

    // #[test]
    // fn test_instantiate_custom_types() {
    //     let engine = ExecutionEngine::new();
        
    //     // Create custom type definition
    //     let mut types = HashMap::new();
    //     types.insert("WeatherConfig".to_string(), json!({
    //         "location_name": {
    //             "type": "string",
    //             "description": "The name of the location",
    //             "required": true
    //         },
    //         "api_key": {
    //             "type": "string",
    //             "description": "API key for the service",
    //             "required": true
    //         },
    //         "temperature_unit": {
    //             "type": "string",
    //             "description": "Temperature unit (celsius/fahrenheit)",
    //             "required": false
    //         }
    //     }));
        
    //     let io_definitions = vec![
    //         ShIO {
    //             name: "weather_config".to_string(),
    //             r#type: "WeatherConfig".to_string(),
    //             template: json!({}),
    //             value: None,
    //             required: true,
    //         }
    //     ];
        
    //     // Test case 1: Valid custom type
    //     let valid_io_values = vec![json!({
    //         "location_name": "Rome",
    //         "api_key": "abc123",
    //         "temperature_unit": "celsius"
    //     })];
        
    //     let result = engine.instantiate(&Some(types.clone()), &io_definitions, &valid_io_values);
    //     assert!(result.is_ok(), "instantiate should succeed for valid custom type");
    //     let instantiated = result.unwrap();
    //     assert_eq!(instantiated.len(), 1);
    //     assert_eq!(instantiated[0], valid_io_values[0]);
        
    //     // Test case 2: Valid custom type with missing optional field
    //     let valid_io_values_minimal = vec![json!({
    //         "location_name": "Paris",
    //         "api_key": "def456"
    //     })];
        
    //     let result = engine.instantiate(&Some(types.clone()), &io_definitions, &valid_io_values_minimal);
    //     assert!(result.is_ok(), "instantiate should succeed for valid custom type with missing optional field");
    //     let instantiated = result.unwrap();
    //     assert_eq!(instantiated.len(), 1);
    //     assert_eq!(instantiated[0], valid_io_values_minimal[0]);
    // }

    // #[test]
    // fn test_instantiate_validation_errors() {
    //     let engine = ExecutionEngine::new();
        
    //     // Create custom type definition
    //     let mut types = HashMap::new();
    //     types.insert("WeatherConfig".to_string(), json!({
    //         "location_name": {
    //             "type": "string",
    //             "description": "The name of the location",
    //             "required": true
    //         },
    //         "api_key": {
    //             "type": "string",
    //             "description": "API key for the service",
    //             "required": true
    //         }
    //     }));
        
    //     let io_definitions = vec![
    //         ShIO {
    //             name: "weather_config".to_string(),
    //             r#type: "WeatherConfig".to_string(),
    //             template: json!({}),
    //             value: None,
    //             required: true,
    //         }
    //     ];
        
    //     // Test case 1: Missing required field
    //     let invalid_io_values = vec![json!({
    //         "location_name": "Rome"
    //         // Missing required api_key field
    //     })];
        
    //     let result = engine.instantiate(&Some(types.clone()), &io_definitions, &invalid_io_values);
    //     assert!(result.is_err(), "instantiate should fail for missing required field");
        
    //     // Test case 2: Wrong field type
    //     let invalid_io_values_type = vec![json!({
    //         "location_name": 123, // Should be string, not number
    //         "api_key": "abc123"
    //     })];
        
    //     let result = engine.instantiate(&Some(types.clone()), &io_definitions, &invalid_io_values_type);
    //     assert!(result.is_err(), "instantiate should fail for wrong field type");
        
    //     // Test case 3: Extra fields not allowed (strict validation)
    //     let invalid_io_values_extra = vec![json!({
    //         "location_name": "Rome",
    //         "api_key": "abc123",
    //         "extra_field": "not_allowed" // This should cause validation to fail
    //     })];
        
    //     let result = engine.instantiate(&Some(types.clone()), &io_definitions, &invalid_io_values_extra);
    //     assert!(result.is_err(), "instantiate should fail for extra fields not in schema");
    // }

    // #[test]
    // fn test_instantiate_no_types() {
    //     let engine = ExecutionEngine::new();
        
    //     let io_definitions = vec![
    //         ShIO {
    //             name: "test_string".to_string(),
    //             r#type: "string".to_string(), // Use primitive type
    //             template: json!(""),
    //             value: None,
    //             required: true,
    //         }
    //     ];
    //     let io_values = vec![json!("hello world")];
    //     let types = None; // No types provided
        
    //     let result = engine.instantiate(&types, &io_definitions, &io_values);
    //     assert!(result.is_ok(), "instantiate should succeed when no types are provided");
    //     let instantiated = result.unwrap();
    //     assert_eq!(instantiated.len(), 1);
    //     assert_eq!(instantiated[0], json!("hello world"));
    // }

    // #[test]
    // fn test_instantiate_mixed_types() {
    //     let engine = ExecutionEngine::new();
        
    //     // Create custom type definition
    //     let mut types = HashMap::new();
    //     types.insert("WeatherConfig".to_string(), json!({
    //         "location_name": {
    //             "type": "string",
    //             "description": "The name of the location",
    //             "required": true
    //         },
    //         "api_key": {
    //             "type": "string",
    //             "description": "API key for the service",
    //             "required": true
    //         }
    //     }));
        
    //     let io_definitions = vec![
    //         ShIO {
    //             name: "location".to_string(),
    //             r#type: "string".to_string(), // Primitive type
    //             template: json!(""),
    //             value: None,
    //             required: true,
    //         },
    //         ShIO {
    //             name: "weather_config".to_string(),
    //             r#type: "WeatherConfig".to_string(), // Custom type
    //             template: json!({}),
    //             value: None,
    //             required: true,
    //         },
    //         ShIO {
    //             name: "temperature".to_string(),
    //             r#type: "number".to_string(), // Primitive type
    //             template: json!(0),
    //             value: None,
    //             required: true,
    //         }
    //     ];
        
    //     let io_values = vec![
    //         json!("Rome"), // For string type
    //         json!({ // For custom type
    //             "location_name": "Rome",
    //             "api_key": "abc123"
    //         }),
    //         json!(25.5) // For number type
    //     ];
        
    //     let result = engine.instantiate(&Some(types), &io_definitions, &io_values);
    //     assert!(result.is_ok(), "instantiate should succeed for mixed types");
    //     let instantiated = result.unwrap();
    //     assert_eq!(instantiated.len(), 3);
    //     assert_eq!(instantiated[0], json!("Rome"));
    //     assert_eq!(instantiated[1], json!({
    //         "location_name": "Rome",
    //         "api_key": "abc123"
    //     }));
    //     assert_eq!(instantiated[2], json!(25.5));
    // }

    // #[test]
    // fn test_instantiate_empty_inputs() {
    //     let engine = ExecutionEngine::new();
        
    //     let io_definitions = vec![];
    //     let io_values = vec![];
    //     let types = None;
        
    //     let result = engine.cast(&types, &io_definitions, &io_values);
    //     assert!(result.is_ok(), "instantiate should succeed for empty inputs");
    //     let instantiated = result.unwrap();
    //     assert_eq!(instantiated.len(), 0);
    // }

    // #[test]
    // #[should_panic(expected = "called `Option::unwrap()` on a `None` value")]
    // fn test_instantiate_mismatched_lengths() {
    //     let engine = ExecutionEngine::new();
        
    //     let io_definitions = vec![
    //         ShIO {
    //             name: "test1".to_string(),
    //             r#type: "string".to_string(),
    //             template: json!(""),
    //             value: None,
    //             required: true,
    //         },
    //         ShIO {
    //             name: "test2".to_string(),
    //             r#type: "string".to_string(),
    //             template: json!(""),
    //             value: None,
    //             required: true,
    //         }
    //     ];
    //     let io_values = vec![json!("only_one_value")]; // Only one value for two definitions
        
    //     let _result = engine.cast(&None, &io_definitions, &io_values);
    //     // This should panic due to unwrap() in the method when trying to access the second value
    // }

    #[tokio::test]
    async fn test_execute_action_http_get_wasm() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing the http-get-wasm action directly
        let action_ref = "starthubhq/http-get-wasm:0.0.16";
        
        // Test with URL and optional headers
        let inputs = vec![
            json!("https://api.restful-api.dev/objects"),  // URL
            json!({  // Headers
                "Accept": "application/json"
            })
        ];
        
        println!("Testing http-get-wasm action with inputs: {:#?}", inputs);
        let result = engine.execute_action(action_ref, inputs).await;
        
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid http-get-wasm action_ref and inputs");
        
        // let outputs = result.unwrap();
        
        // // Verify that we got outputs
        // assert!(outputs.is_array(), "execute_action should return an array of outputs");
        // let outputs_array = outputs.as_array().unwrap();
        
        // // For a WASM action, we expect at least one output
        // assert!(!outputs_array.is_empty(), "WASM action should produce outputs");
        
        // // Verify the output structure - should be an HTTP response
        // let first_output = &outputs_array[0];
        // assert!(first_output.is_object(), "Output should be an object");
        
        // // Check if it's a valid HTTP response structure
        // if let Some(response_obj) = first_output.as_object() {
        //     // HTTP response should have status, headers, and body
        //     assert!(response_obj.contains_key("status") || response_obj.contains_key("body") || 
        //            response_obj.contains_key("headers"), 
        //            "HTTP response should contain status, body, or headers");
        // }
    }

    #[tokio::test]
    async fn test_execute_action_get_simulator_by_id() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing action with simulator_id input
        let action_ref = "starthubhq/get-simulator-by-id:0.0.1";
        let inputs = vec![
            json!("1"),  // simulator_id as string
            json!("sb_publishable_AKGy20M54_uMOdJme3ZnZA_GX11LgHe")  // api_key as string
        ];
        
        let result = engine.execute_action(action_ref, inputs).await;
        
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid action_ref and inputs");
        
        let outputs = result.unwrap();
        println!("outputs: {:#?}", outputs);
        // The function returns an array of output values directly
        assert!(outputs.is_array());
        let outputs_array = outputs.as_array().unwrap();
        assert_eq!(outputs_array.len(), 1);
    
        let output = &outputs_array[0];
        assert!(output.is_object());
        let output_obj = output.as_object().unwrap();
        
        // Verify the output has the expected structure with id and value
        assert!(output_obj.contains_key("id"));
        assert!(output_obj.contains_key("value"));
        assert_eq!(output_obj["id"], 1);
        // Value should be a string (could be empty if simulator doesn't exist)
        assert!(output_obj["value"].is_number());
    }

    #[tokio::test]
    async fn test_execute_action_poll_simulator_state_by_id() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing the poll simulator state action
        let action_ref = "starthubhq/poll-simulator-state-by-id:0.0.1";
        
        // Test with simulator_id and api_key inputs
        let inputs = vec![
            json!("1"),  // simulator_id as string
            json!("sb_publishable_AKGy20M54_uMOdJme3ZnZA_GX11LgHe")  // api_key as string
        ];
        
        let result = engine.execute_action(action_ref, inputs).await;
        
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid poll simulator state action_ref and inputs");
        
        let action_tree = result.unwrap();
        
        // Verify the action structure
        assert_eq!(action_tree["kind"], "composition");
        assert_eq!(action_tree["uses"], action_ref);
        
        // Verify the composition has steps
        let steps = &action_tree["steps"];
        assert!(steps.is_object(), "steps should be an object");
        
        let steps_obj = steps.as_object().unwrap();
        assert!(steps_obj.contains_key("get_simulator"), "should contain get_simulator step");
        assert!(steps_obj.contains_key("sleep"), "should contain sleep step");
        
        // Verify get_simulator step uses the correct action
        let get_simulator_step = &steps_obj["get_simulator"];
        assert_eq!(get_simulator_step["uses"], "starthubhq/get-simulator-by-id:0.0.1");
        
        // Verify sleep step uses the correct action
        let sleep_step = &steps_obj["sleep"];
        assert_eq!(sleep_step["uses"], "std/sleep:0.0.1");
        
        // Verify sleep step has flow control inputs
        let sleep_inputs = &sleep_step["inputs"];
        assert!(sleep_inputs.is_array(), "sleep inputs should be an array");
        
        let sleep_inputs_array = sleep_inputs.as_array().unwrap();
        assert_eq!(sleep_inputs_array.len(), 3, "sleep should have 3 inputs");
        
        // Check seconds input
        let seconds_input = &sleep_inputs_array[0];
        assert_eq!(seconds_input["name"], "seconds");
        assert_eq!(seconds_input["value"], 5);
        
        // Check next_step input
        let next_step_input = &sleep_inputs_array[1];
        assert_eq!(next_step_input["name"], "next_step");
        assert_eq!(next_step_input["value"], "get_simulator");
        
        // Check depends_on input
        let depends_on_input = &sleep_inputs_array[2];
        assert_eq!(depends_on_input["name"], "depends_on");
        assert_eq!(depends_on_input["value"], "{{steps.get_simulator.outputs[0].id}}");
    }
    

    
}