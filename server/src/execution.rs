use anyhow::Result;
use jsonschema::JSONSchema;
use serde_json::Value;
use tracing_subscriber::filter::combinator::Or;
use std::collections::HashMap;
use dirs;
use tokio::sync::broadcast;

use crate::models::{ShManifest, ShKind, ShIO, ShAction, ShRole};
use crate::{docker, wasm};
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

    pub async fn execute_action(&mut self, action_ref: &str, input_values: Vec<Value>) -> Result<Value> {
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
        let typed_array_to_inject = self.cast_values_to_typed_array(
            &root_action.inputs,
            &input_values, 
            &root_action.types)?;
        
        // Create a new action with injected inputs (avoiding deep clone)
        let new_root_action = ShAction {
            inputs: typed_array_to_inject,
            ..root_action
        };        
        
        self.logger.log_success("Action tree built successfully", Some(&new_root_action.id));

        self.logger.log_info("Executing action tree...", Some(&new_root_action.id));
        let executed_action = self.run_action_tree(&new_root_action).await?;
        
        self.logger.log_success("Action execution completed", Some(&new_root_action.id));

        // Extract outputs from the executed action
        let output_values: Vec<Value> = executed_action.outputs.iter()
            .map(|io| io.value.clone().unwrap_or(Value::Null))
            .collect();

        // Return the outputs directly
        Ok(serde_json::to_value(output_values)?)
    }

    async fn run_action_tree(&mut self, action: &ShAction) -> Result<ShAction> {
        // Base condition.
        if action.kind == "wasm" || action.kind == "docker" {
            self.logger.log_info(&format!("Executing {} wasm step: {}", action.kind, action.name), Some(&action.id));

            // Extract values from inputs before serializing
            let input_values_to_serialise: Vec<Value> = action.inputs.iter()
                .map(|io| io.value.clone().unwrap_or(Value::Null))
                .collect();

            let result = if action.kind == "wasm" {
                wasm::run_wasm_step(
                    action, 
                    &serde_json::to_value(&input_values_to_serialise)?, 
                    &self.cache_dir,
                    &|msg, id| self.logger.log_info(msg, id),
                    &|msg, id| self.logger.log_success(msg, id),
                    &|msg, id| self.logger.log_error(msg, id),
                ).await?
            } else if action.kind == "docker" {
                docker::run_docker_step(
                    action,
                    &serde_json::to_value(&input_values_to_serialise)?,
                    &self.cache_dir,
                    &|msg, id| self.logger.log_info(msg, id),
                    &|msg, id| self.logger.log_success(msg, id),
                    &|msg, id| self.logger.log_error(msg, id),
                ).await?
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
            let typed_updated_outputs = self.cast_values_to_typed_array(
                &action.outputs,
                &json_objects,
                &action.types
            )?;

            // Create a new action with the updated outputs.
            let updated_action = ShAction {
                outputs: typed_updated_outputs,
                ..action.clone()
            };
            
            return Ok(updated_action);
        }

        let mut execution_buffer: Vec<String> = Vec::new();

        // Initially, we want to inject the input values into the inputs of the steps.
        // This will help us understand what steps are ready to be executed.
        let steps_with_injected_inputs: HashMap<String, ShAction> = self.recalculate_steps(
            &action.inputs, 
            &action.steps
        );

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
        
        // Now we can start the iterative execution of steps
        // Using a loop-based approach instead of recursion to avoid stack overflow
        let mut current_action = action_with_inputs_resolved_into_steps;
        let mut current_execution_buffer = execution_buffer;
        
        // Iterative execution loop
        while !current_execution_buffer.is_empty() {
            // Get the first step from the buffer
            let current_step_id = current_execution_buffer.first().unwrap().clone();
            println!("current_step_id: {}", current_step_id);
            
            // Remove the first step from the buffer
            let remaining_buffer = current_execution_buffer.into_iter().skip(1).collect::<Vec<String>>();
            
            // Execute the current step
            if let Some(step) = current_action.steps.get(&current_step_id) {
                // Since the step is coming from the execution buffer, it means that
                // it is ready to be executed.
                // Execute the step
                let executed_step = Box::pin(self.run_action_tree(step)).await?;

                // Substitute the step in the current action with the executed step
                let updated_steps: HashMap<String, ShAction> = current_action.steps.iter()
                    .map(|(id, step)| {
                        if id == &current_step_id {
                            (id.clone(), executed_step.clone())
                        } else {
                            (id.clone(), step.clone())
                        }
                    })
                    .collect();

                
                let current_action_with_updated_steps = ShAction {
                    steps: updated_steps,
                    ..current_action.clone()
                };

                // By the time we get here, the current action has been updated with the outputs of the step we have just executed.
                // However, the effects of the processing of the current step have not beem applied to the siblings yet.
                // For each sibling, inject the outputs of the step we have just executed
                // into the inputs of the dependent step                
                let recalculated_steps: HashMap<String, ShAction> = self.recalculate_steps(
                    &current_action_with_updated_steps.inputs, 
                    &current_action_with_updated_steps.steps
                );

                let updated_current_action = ShAction {
                    steps: recalculated_steps,
                    ..current_action_with_updated_steps.clone()
                };
                
                // Create new buffer by combining remaining steps with new downstream steps
                let mut new_execution_buffer = remaining_buffer;
                if !new_execution_buffer.contains(&"outputs".to_string()) {
                    // Find the ready steps that are directly downstream of the step we just executed
                    let downstream_step_ids = self.find_next_step_id(
                            &updated_current_action.steps,
                            &current_step_id,
                        &updated_current_action.inputs,
                        &updated_current_action.outputs
                    )?;

                    for step_id in downstream_step_ids {
                        self.push_to_execution_buffer(&mut new_execution_buffer, step_id);
                    }
                } 
                
                    
                // Update the current state for the next iteration
                current_action = updated_current_action;
                current_execution_buffer = new_execution_buffer;
            } else {
                // If step not found, continue with remaining buffer
                current_execution_buffer = remaining_buffer;
            }
        }
        
        // The outputs could be coming from the parent inputs or the sibling steps.
        let resolved_untyped_outputs = self.resolve_untyped_output_values(
            &action.outputs,
            &action.inputs,
            &current_action.steps
        )?;

        // Create a new action with resolved outputs
        let updated_action = ShAction {
            steps: current_action.steps,
            outputs: self.cast_values_to_typed_array(
                &action.outputs,
                &resolved_untyped_outputs,
                &action.types
            )?,
            ..action.clone()
        };

        Ok(updated_action.clone())
    }

    /// Instantiates and assigns values to IO fields in one operation
    fn cast_values_to_typed_array(
        &self,
        io_fields: &Vec<ShIO>,
        io_values: &Vec<Value>,
        types: &Option<serde_json::Map<String, Value>>
    ) -> Result<Vec<ShIO>> {
        // println!("casting values to typed array");
        // println!("io_fields: {:#?}", io_fields);
        // println!("io_values: {:#?}", io_values);
        // println!("types: {:#?}", types);
        let mut cast_values: Vec<Value> = Vec::new();
        // For each IO field, cast the value to the appropriate type
        for (index, io) in io_fields.iter().enumerate() {
            let value_to_inject = io_values.get(index).unwrap().clone();
            
            let converted_value = self.cast(&value_to_inject, &io.r#type, types)?;
            cast_values.push(converted_value);
        }

        // Inject the cast values into the IO array
        let io_array = io_fields.iter()
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

        Ok(io_array)
    }

    /// Casts a single value to the appropriate type
    fn cast(&self,
        value: &Value,
        target_type: &str,
        available_types: &Option<serde_json::Map<String, Value>>
    ) -> Result<Value> {
        // println!("casting value: {:#?}", value);
        // println!("target_type: {:#?}", target_type);
        // println!("available_types: {:#?}", available_types);
        // Handle primitive types with explicit conversion
        if target_type == "string" || 
            target_type == "bool" ||
            target_type == "number" ||
            target_type == "object" ||
            target_type == "id" {
            
            let converted_value = match target_type {
                "id" => value.clone(),
                "string" => value.clone(),
                "number" => {
                    // Convert string to number if needed
                    match value {
                        Value::String(s) => {
                            if let Ok(n) = s.parse::<f64>() {
                                Value::Number(serde_json::Number::from_f64(n).unwrap_or(serde_json::Number::from(0)))
                            } else {
                                return Err(anyhow::anyhow!("Cannot convert string '{}' to number", s));
                            }
                        },
                        Value::Number(n) => Value::Number(n.clone()),
                        _ => return Err(anyhow::anyhow!("Cannot convert {:?} to number", value)),
                    }
                },
                "bool" => {
                    // Convert string to boolean if needed
                    match value {
                        Value::String(s) => {
                            match s.as_str() {
                                "true" => Value::Bool(true),
                                "false" => Value::Bool(false),
                                _ => return Err(anyhow::anyhow!("Cannot convert string '{}' to boolean", s)),
                            }
                        },
                        Value::Bool(b) => Value::Bool(*b),
                        _ => return Err(anyhow::anyhow!("Cannot convert {:?} to boolean", value)),
                    }
                },
                "object" => value.clone(),
                _ => value.clone(),
            };
            
            Ok(converted_value)
        } else {
            // Look up type definition
            let type_definition = if target_type == "string" || 
                target_type == "bool" ||
                target_type == "number" ||
                target_type == "object" {
                None // Primitive types don't need type definition lookup
            } else if target_type == "id" {
                Some(&Value::String("string".to_string()))
            } else {
                // Look up custom type definition
                available_types.as_ref()
                    .and_then(|types_map| types_map.get(target_type))
            };

            // Handle custom types
            if let Some(type_def) = type_definition {
                let json_schema = match self.convert_to_json_schema(type_def) {
                        Ok(schema) => schema,
                        Err(e) => {
                            return Err(anyhow::anyhow!("Failed to convert type definition: {}", e));
                        }
                    };

                    // Compile the JSON schema
                    let compiled_schema = match JSONSchema::compile(&json_schema) {
                        Ok(schema) => schema,
                        Err(e) => {
                        return Err(anyhow::anyhow!("Failed to compile schema for type '{}': {}", target_type, e));
                    }
                };

                // Validate the value against the schema
                if compiled_schema.validate(value).is_ok() {
                    Ok(value.clone())
                    } else {
                    let error_list: Vec<_> = compiled_schema.validate(value).unwrap_err().collect();
                    return Err(anyhow::anyhow!("Value is invalid: {:?}", error_list));
                    }
                } else {
                // No type definition provided - pass through unchanged
                Ok(value.clone())
                }
            }
    }

    fn resolve_untyped_output_values(&self,
        outputs: &Vec<ShIO>,
        inputs: &Vec<ShIO>,
        children: &HashMap<String, ShAction>
    ) -> Result<Vec<Value>> {
        // Extract values from the inputs vector
        let input_values: Vec<Value> = inputs.iter()
            .map(|io| io.value.clone().unwrap_or(Value::Null))
            .collect();

        // For every output, we want to interpolate the template into the value
        let resolved_outputs: Result<Vec<Value>> = outputs.iter()
            .map(|output| {
                self.interpolate_into_untyped_value(&output.template, &input_values, Some(children))
            })
            .collect();
        
        let resolved_outputs = resolved_outputs?;

        Ok(resolved_outputs)
    }

    fn recalculate_steps(&self,
        inputs: &Vec<ShIO>,
        children: &HashMap<String, ShAction>) -> HashMap<String, ShAction> {
        
        // Extract values from the inputs vector
        let values: Vec<Value> = inputs.iter()
            .map(|io| io.value.clone().unwrap_or(Value::Null))
            .collect();
            
        children.iter()
            .map(|(step_id, step)| {
                // For every input of every child, iterate through the input definitions
                // and resolve the template to get the actual value
                let resolved_untyped_values: Result<Vec<Value>, ()> = step.inputs.iter()
                    .map(|definition| {
                        // Resolve the template to get the actual value
                        self.interpolate_into_untyped_value(&definition.template, &values, Some(children))
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

                // Once we have resolved the inputs we want to create a new array of typed inputs to inject into the child step
                if let Some(resolved_inputs_to_inject_into_child_step) = resolved_untyped_values.ok() {
                    let inputs_array_to_inject = self.cast_values_to_typed_array(
                        &step.inputs, 
                        &resolved_inputs_to_inject_into_child_step,
                        &step.types
                    ).ok();
                    
                    if let Some(inputs_to_inject) = inputs_array_to_inject {
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

    fn interpolate_into_untyped_value(&self, 
        template: &Value, 
        inputs: &Vec<Value>,
        executed_steps: Option<&HashMap<String, ShAction>>,
    ) -> Result<Value> {
        // println!("Interpolating from parent inputs: {:#?}", template);
        // println!("Variables: {:#?}", variables);
        match template {
            Value::String(s) => {
                // println!("resolve_template_string: {:#?}", s);
                let resolved = self.interpolate_string_into_untyped_value(s, inputs, executed_steps)?;
                Ok(resolved)
            },
            Value::Object(obj) => {
                // Recursively resolve object templates
                let mut resolved_obj = serde_json::Map::new();
                for (key, value) in obj {
                    let resolved_value = self.interpolate_into_untyped_value(value, inputs, executed_steps)?;
                    resolved_obj.insert(key.clone(), resolved_value);
                }
                
                Ok(Value::Object(resolved_obj))
            },
            Value::Array(arr) => {
                // println!("resolve_template_array: {:#?}", arr);
                // Recursively resolve array templates
                let mut resolved_arr = Vec::new();
                for item in arr {
                    let resolved_item = self.interpolate_into_untyped_value(item, inputs, executed_steps)?;
                    resolved_arr.push(resolved_item);
                }
                Ok(Value::Array(resolved_arr))
            },
            _ => Ok(template.clone())
        }
    }


    fn interpolate_string_into_untyped_value(&self, 
        template: &str, 
        variables: &Vec<Value>,
        executed_steps: Option<&HashMap<String, ShAction>>,
    ) -> Result<Value> {
        // Check for simple direct input reference (no string interpolation needed)
        let simple_re = regex::Regex::new(r"^\{\{inputs\[(\d+)\]\}\}$")?;
        if let Some(cap) = simple_re.captures(template) {
            if let Some(index_str) = cap.get(1) {
                if let Ok(index) = index_str.as_str().parse::<usize>() {
                    if let Some(input_value) = variables.get(index) {
                        return Ok(input_value.clone());
                    }
                }
            }
        }
        
        // Check for simple input jsonpath reference
        let jsonpath_re = regex::Regex::new(r"^\{\{inputs\[(\d+)\]\.([^}]+)\}\}$")?;
        if let Some(cap) = jsonpath_re.captures(template) {
            if let (Some(index_str), Some(jsonpath)) = (cap.get(1), cap.get(2)) {
                if let Ok(index) = index_str.as_str().parse::<usize>() {
                    if let Some(input_value) = variables.get(index) {
                        if let Ok(resolved_value) = self.evaluate_jsonpath(input_value, jsonpath.as_str()) {
                            return Ok(resolved_value);
                        }
                    }
                }
            }
        }
        
        // Fallback to string interpolation for complex templates
        let simple_re = regex::Regex::new(r"\{\{inputs\[(\d+)\]\}\}")?;
        let result = simple_re.captures_iter(template)
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
        
        let jsonpath_re = regex::Regex::new(r"\{\{inputs\[(\d+)\]\.([^}]+)\}\}")?;
        let result = jsonpath_re.captures_iter(&result.clone())
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
        
        // Handle sibling step outputs: {{steps.step_name.outputs[index]}}
        if let Some(executed_steps) = executed_steps {
            // Check for simple direct step output reference (no string interpolation needed)
            let steps_simple_re = regex::Regex::new(r"^\{\{steps\.([^.]+)\.outputs\[(\d+)\]\}\}$")?;
            if let Some(cap) = steps_simple_re.captures(template) {
                if let (Some(step_name), Some(index_str)) = (cap.get(1), cap.get(2)) {
                    if let Ok(index) = index_str.as_str().parse::<usize>() {
                        if let Some(step) = executed_steps.get(step_name.as_str()) {
                            if let Some(output) = step.outputs.get(index) {
                                if let Some(output_value) = &output.value {
                                    return Ok(output_value.clone());
                                }
                            }
                        }
                    }
                }
            }
            
            // Check for simple step output jsonpath reference
            let steps_jsonpath_re = regex::Regex::new(r"^\{\{steps\.([^.]+)\.outputs\[(\d+)\]\.([^}]+)\}\}$")?;
            if let Some(cap) = steps_jsonpath_re.captures(template) {
                if let (Some(step_name), Some(index_str), Some(jsonpath)) = (cap.get(1), cap.get(2), cap.get(3)) {
                    if let Ok(index) = index_str.as_str().parse::<usize>() {
                        if let Some(step) = executed_steps.get(step_name.as_str()) {
                            if let Some(output) = step.outputs.get(index) {
                                if let Some(output_value) = &output.value {
                                    if let Ok(resolved_value) = self.evaluate_jsonpath(output_value, jsonpath.as_str()) {
                                        return Ok(resolved_value);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        
        Ok(Value::String(result))
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
                // Try to parse as JSON, but preserve string values that look like numbers
                if let Ok(parsed) = serde_json::from_str::<Value>(&s) {
                    // Check if the parsed value is a number that was originally a string
                    if let Value::Number(n) = &parsed {
                        // If it's a simple number that could be a semantic version, keep it as string
                        if let Some(num) = n.as_f64() {
                            if num.fract() == 0.0 && num >= 1.0 && num <= 99.0 {
                                // This looks like a semantic version, keep as string
                                return Value::String(s);
                            }
                        }
                    }
                    Self::parse(parsed)
            } else {
                    Value::String(s)
            }
            },
            _ => value
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
            // TODO: find a way to determine priority at build time
            priority: 0,
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
                                    if let Some(child_input) = child_action.inputs.get_mut(index) {
                                    // Handle both formats:
                                    // 1. New format: direct values (string, object, etc.)
                                    // 2. Old format: objects with "value" property
                                    let template_value = if let Some(input_obj) = input.as_object() {
                                        // Old format: object with "value" property
                                        if let Some(value) = input_obj.get("value") {
                                            value.clone()
                                        } else {
                                            input.clone()
                                        }
                                    } else {
                                        // New format: direct value
                                        input.clone()
                                    };
                                    
                                    child_input.template = template_value;
                                }
                            }
                        }
                    }

                    
                    // Add child to parent's children HashMap
                    action_state.steps.insert(_step_name.clone(), child_action);
                }
            }
        }
        
        // After creating the action tree, we want to calculate the priority of the action
        let steps_with_priorities = self.produce_steps_with_priorities(&action_state.steps);

        action_state = ShAction {
            steps: steps_with_priorities,
            ..action_state.clone()
        };

        return Ok(action_state);
    }

    fn produce_steps_with_priorities(&self, steps: &HashMap<String, ShAction>) -> HashMap<String, ShAction>{
        // Sort step keys alphabetically to ensure deterministic ordering
        let mut sorted_keys: Vec<_> = steps.keys().collect();
        sorted_keys.sort();
        
        // Create new steps with priorities assigned based on alphabetical order
        let mut steps_with_priorities = HashMap::new();
        for (index, step_key) in sorted_keys.iter().enumerate() {
            if let Some(step) = steps.get(*step_key) {
                let mut updated_step = step.clone();
                updated_step.priority = index as i32;
                steps_with_priorities.insert((*step_key).clone(), updated_step);
            }
        }
        
        steps_with_priorities
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

            if all_inputs_resolved {
                ready_steps.push(step_id.clone());
            }
        }
        
        Ok(ready_steps)
    }

    /// Finds the first step that depends on a completed step and is now ready to execute
    fn find_next_step_id(
        &self,
        steps: &HashMap<String, ShAction>,
        completed_step_id: &str,
        parent_inputs: &Vec<ShIO>,
        outputs: &Vec<ShIO>,
    ) -> Result<Vec<String>> {
        // Find all steps that depend on the completed step and are ready
        // Sort steps by priority (lower priority number = higher priority)
        let mut sorted_steps: Vec<_> = steps.iter().collect();
        sorted_steps.sort_by(|(_, a), (_, b)| {
            a.priority.cmp(&b.priority)
        });

        let mut downstream_steps = Vec::new();

        // if the current step is a flow control step, we want to find it among the steps, 
        // get the next step by using the first output of the step we have just executed.
        if let Some((_, step)) = sorted_steps.iter().find(|(id, _)| id == &completed_step_id) {
            if step.role.as_ref().map_or(false, |r| r == &ShRole::FlowControl) {
                
                if let Some(output) = step.outputs.first() {
                    if let Some(output_value) = &output.value {
                        if let Some(output_value_str) = output_value.as_str() {
                            let next_step_id = output_value_str;
                            // Check if the step exists in the steps vector
                            // if steps.contains_key(next_step_id) {
                            downstream_steps.push(next_step_id.to_string());
                            // }
                        }
                    }
                }
            }
        }
            
        for (step_id, step) in sorted_steps {
            // Skip if this is the step we just completed
            if step_id == completed_step_id {
                continue;
            }

            let depends_on = self.step_depends_on(step, completed_step_id);
            let is_ready = self.are_all_inputs_ready(step, &step.inputs)?;

            // println!("step_id: {:#?}", step_id);
            // println!("depends_on: {:#?}", depends_on);
            // println!("is_ready: {:#?}", is_ready);
            // println!("step: {:#?}", step.inputs);
            // println!("--------------------------------");
            // Check if this step depends on the completed step and is now ready
            if depends_on && is_ready {
                // println!("found next step id from regular step from current step id: {:#?} to {:#?}", completed_step_id, step_id);
                downstream_steps.push(step_id.clone());
            }
        }
        
        // println!("downstream_steps: {:#?}", downstream_steps);

        // let resolved_outputs = self.resolve_untyped_output_values(outputs, parent_inputs, steps)?;
        // let all_outputs_have_values = steps.values().all(|step| 
        //     step.outputs.iter().all(|output| output.value.is_some())
        // );

        // println!("All outputs have values: {:#?}", all_outputs_have_values);
        // If the length of the resolved outputs is the same as the length of the outputs, then we have a complete match
        // if resolved_outputs.len() == outputs.len() && all_outputs_have_values {
        //     println!("completed_step_id: {:#?}", completed_step_id);
        //     println!("resolved_outputs: {:#?}", resolved_outputs);
        //     println!("outputs: {:#?}", outputs);
            
        //     // downstream_steps.push("outputs".to_string());
        // }

        Ok(downstream_steps)
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
            Err(anyhow::anyhow!("Failed to download starthub-lock.json: {} from url: {}", response.status(), storage_url))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    use serde_json::json;

    // #[test]
    // fn test_step_depends_on() {
    //     // Create a mock ExecutionEngine
    //     let engine = ExecutionEngine::new();
        
    //     // Test case 1: Step depends on another step (positive case)
    //     let step_with_dependency = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "string".to_string(),
    //                 template: Value::String("steps.step2.output".to_string()),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(engine.step_depends_on(&step_with_dependency, "step2"));
    //     assert!(!engine.step_depends_on(&step_with_dependency, "step3"));
        
    //     // Test case 2: Step does not depend on any step (negative case)
    //     let step_without_dependency = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "string".to_string(),
    //                 template: Value::String("static_value".to_string()),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(!engine.step_depends_on(&step_without_dependency, "step2"));
        
    //     // Test case 3: Step with multiple inputs, one depends on another step
    //     let step_with_multiple_inputs = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         priority: 0,
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "string".to_string(),
    //                 template: Value::String("static_value".to_string()),
    //                 value: None,
    //                 required: true,
    //             },
    //             ShIO {
    //                 name: "input2".to_string(),
    //                 r#type: "string".to_string(),
    //                 template: Value::String("steps.step3.result".to_string()),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(engine.step_depends_on(&step_with_multiple_inputs, "step3"));
    //     assert!(!engine.step_depends_on(&step_with_multiple_inputs, "step2"));
        
    //     // Test case 4: Step with non-string template (should not match)
    //     let step_with_non_string_template = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         priority: 0,
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "number".to_string(),
    //                 template: Value::Number(serde_json::Number::from(42)),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(!engine.step_depends_on(&step_with_non_string_template, "step2"));
        
    //     // Test case 5: Step with empty inputs
    //     let step_with_empty_inputs = ShAction {
    //         id: "step1".to_string(),
    //         priority: 0,
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![],
    //         outputs: vec![],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(!engine.step_depends_on(&step_with_empty_inputs, "step2"));
        
    //     // Test case 6: Step with partial match in template (should match because contains() is used)
    //     let step_with_partial_match = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         priority: 0,
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "string".to_string(),
    //                 template: Value::String("some_steps.step2.other".to_string()),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(engine.step_depends_on(&step_with_partial_match, "step2"));
        
    //     // Test case 7: Step with exact match format
    //     let step_with_exact_match = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "string".to_string(),
    //                 template: Value::String("steps.step2".to_string()),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         priority: 0,
    //         outputs: vec![],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(engine.step_depends_on(&step_with_exact_match, "step2"));
        
    //     // Test case 8: Step with no dependency (true negative case)
    //     let step_with_no_dependency = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "string".to_string(),
    //                 template: Value::String("completely_different_string".to_string()),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         parent_action: None,
    //         priority: 0,
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "object".to_string(),
    //                 template: Value::Object({
    //                     let mut map = serde_json::Map::new();
    //                     map.insert("url".to_string(), Value::String("https://api.example.com/data?q={{steps.step2.result}}".to_string()));
    //                     map.insert("headers".to_string(), Value::Object({
    //                         let mut headers_map = serde_json::Map::new();
    //                         headers_map.insert("Content-Type".to_string(), Value::String("application/json".to_string()));
    //                         headers_map.insert("Authorization".to_string(), Value::String("Bearer {{steps.step2.token}}".to_string()));
    //                         headers_map
    //                     }));
    //                     map
    //                 }),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     // Now that step_depends_on recursively searches objects, this should be true
    //     assert!(engine.step_depends_on(&step_with_object_template, "step2"));
        
    //     // Test case 10: Step with nested object template containing dependency (now matches with recursive search)
    //     let step_with_nested_object_template = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "object".to_string(),
    //                 template: Value::Object({
    //                     let mut map = serde_json::Map::new();
    //                     map.insert("local_names".to_string(), Value::Object({
    //                         let mut inner_map = serde_json::Map::new();
    //                         inner_map.insert("en".to_string(), Value::String("{{steps.step3.outputs[0].body[0].local_names.en}}".to_string()));
    //                         inner_map.insert("it".to_string(), Value::String("{{steps.step3.outputs[0].body[0].local_names.it}}".to_string()));
    //                         inner_map
    //                     }));
    //                     map.insert("lat".to_string(), Value::String("{{steps.step3.outputs[0].body[0].lat}}".to_string()));
    //                     map.insert("lon".to_string(), Value::String("{{steps.step3.outputs[0].body[0].lon}}".to_string()));
    //                     map
    //                 }),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(engine.step_depends_on(&step_with_nested_object_template, "step3"));
    //     assert!(!engine.step_depends_on(&step_with_nested_object_template, "step2"));
        
    //     // Test case 11: Step with array template containing dependency (now matches with recursive search)
    //     let step_with_array_template = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "array".to_string(),
    //                 template: Value::Array(vec![
    //                     Value::String("{{steps.step4.outputs[0].body[0].lat}}".to_string()),
    //                     Value::String("{{steps.step4.outputs[0].body[0].lon}}".to_string()),
    //                     Value::String("{{steps.step4.outputs[0].body[0].country}}".to_string()),
    //                     Value::String("static_value".to_string()),
    //                     Value::Number(serde_json::Number::from(42))
    //                 ]),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         priority: default_priority(),
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(engine.step_depends_on(&step_with_array_template, "step4"));
    //     assert!(!engine.step_depends_on(&step_with_array_template, "step2"));
        
    //     // Test case 12: Step with complex nested structure containing dependency (now matches with recursive search)
    //     let step_with_complex_template = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "object".to_string(),
    //                 template: Value::Object({
    //                     let mut map = serde_json::Map::new();
    //                     map.insert("local_names".to_string(), Value::Object({
    //                         let mut local_names_map = serde_json::Map::new();
    //                         local_names_map.insert("en".to_string(), Value::String("{{steps.step5.outputs[0].body[0].local_names.en}}".to_string()));
    //                         local_names_map.insert("it".to_string(), Value::String("{{steps.step5.outputs[0].body[0].local_names.it}}".to_string()));
    //                         local_names_map.insert("fr".to_string(), Value::String("{{steps.step5.outputs[0].body[0].local_names.fr}}".to_string()));
    //                         local_names_map
    //                     }));
    //                     map.insert("lat".to_string(), Value::String("{{steps.step6.outputs[0].body[0].lat}}".to_string()));
    //                     map.insert("lon".to_string(), Value::String("{{steps.step6.outputs[0].body[0].lon}}".to_string()));
    //                     map.insert("country".to_string(), Value::String("{{steps.step6.outputs[0].body[0].country}}".to_string()));
    //                     map.insert("state".to_string(), Value::String("{{steps.step6.outputs[0].body[0].state}}".to_string()));
    //                     map
    //                 }),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         priority: 0,
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(engine.step_depends_on(&step_with_complex_template, "step5"));
    //     assert!(engine.step_depends_on(&step_with_complex_template, "step6"));
    //     assert!(!engine.step_depends_on(&step_with_complex_template, "step2"));
        
    //     // Test case 13: Step with object template but no dependency
    //     let step_with_object_no_dependency = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "object".to_string(),
    //                 template: Value::Object({
    //                     let mut map = serde_json::Map::new();
    //                     map.insert("lat".to_string(), Value::String("40.7128".to_string()));
    //                     map.insert("lon".to_string(), Value::String("-74.0060".to_string()));
    //                     map.insert("country".to_string(), Value::String("US".to_string()));
    //                     map.insert("state".to_string(), Value::String("NY".to_string()));
    //                     map
    //                 }),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         priority: 0,
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(!engine.step_depends_on(&step_with_object_no_dependency, "step2"));
        
    //     // Test case 14: Step with string template containing JSON-like object (should match)
    //     let step_with_json_string_template = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "string".to_string(),
    //                 template: Value::String(r#"{"source": "steps.step7.data", "type": "json"}"#.to_string()),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         parent_action: None,
    //         priority: 0,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(engine.step_depends_on(&step_with_json_string_template, "step7"));
    //     assert!(!engine.step_depends_on(&step_with_json_string_template, "step2"));
        
    //     // Test case 15: Step with string template containing multiple dependencies
    //     let step_with_multiple_dependencies = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "string".to_string(),
    //                 template: Value::String("steps.step8.output and steps.step9.result".to_string()),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         priority: 0,
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(engine.step_depends_on(&step_with_multiple_dependencies, "step8"));
    //     assert!(engine.step_depends_on(&step_with_multiple_dependencies, "step9"));
    //     assert!(!engine.step_depends_on(&step_with_multiple_dependencies, "step2"));
        
    //     // Test case 16: Mixed inputs - one string with dependency, one object without dependency
    //     let step_with_mixed_inputs = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "string".to_string(),
    //                 template: Value::String("steps.step10.output".to_string()),
    //                 value: None,
    //                 required: true,
    //             },
    //             ShIO {
    //                 name: "input2".to_string(),
    //                 r#type: "object".to_string(),
    //                 template: Value::Object({
    //                     let mut map = serde_json::Map::new();
    //                     map.insert("lat".to_string(), Value::String("{{steps.step11.outputs[0].body[0].lat}}".to_string()));
    //                     map.insert("lon".to_string(), Value::String("{{steps.step11.outputs[0].body[0].lon}}".to_string()));
    //                     map.insert("country".to_string(), Value::String("{{steps.step11.outputs[0].body[0].country}}".to_string()));
    //                     map
    //                 }),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         parent_action: None,
    //         priority: 0,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(engine.step_depends_on(&step_with_mixed_inputs, "step10"));
    //     assert!(engine.step_depends_on(&step_with_mixed_inputs, "step11")); // Object template now checked recursively
    //     assert!(!engine.step_depends_on(&step_with_mixed_inputs, "step2"));
        
    //     // Test case 17: Step with string template using correct {{}} format (like in starthub-lock.json)
    //     let step_with_correct_template_format = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "string".to_string(),
    //                 template: Value::String("https://api.example.com/data?q={{steps.step12.result}}&key={{steps.step13.api_key}}".to_string()),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         priority: 0,
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(engine.step_depends_on(&step_with_correct_template_format, "step12"));
    //     assert!(engine.step_depends_on(&step_with_correct_template_format, "step13"));
    //     assert!(!engine.step_depends_on(&step_with_correct_template_format, "step2"));
        
    //     // Test case 18: Step with object template using correct {{}} format (now matches with recursive search)
    //     let step_with_object_correct_format = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "object".to_string(),
    //                 template: Value::Object({
    //                     let mut map = serde_json::Map::new();
    //                     map.insert("url".to_string(), Value::String("https://api.openweathermap.org/geo/1.0/direct?q={{steps.step14.outputs[0].body[0].location_name}}&limit=1&appid={{steps.step15.outputs[0].body[0].open_weather_api_key}}".to_string()));
    //                     map.insert("headers".to_string(), Value::Object({
    //                         let mut headers_map = serde_json::Map::new();
    //                         headers_map.insert("Content-Type".to_string(), Value::String("application/json".to_string()));
    //                         headers_map.insert("Authorization".to_string(), Value::String("Bearer {{steps.step16.outputs[0].body[0].token}}".to_string()));
    //                         headers_map
    //                     }));
    //                     map
    //                 }),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         parent_action: None,
    //         priority: 0,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(engine.step_depends_on(&step_with_object_correct_format, "step14"));
    //     assert!(engine.step_depends_on(&step_with_object_correct_format, "step15"));
    //     assert!(engine.step_depends_on(&step_with_object_correct_format, "step16"));
        
    //     // Test case 19: Step with number template (should not match)
    //     let step_with_number_template = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "number".to_string(),
    //                 template: Value::Number(serde_json::Number::from(42)),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         parent_action: None,
    //         priority: 0,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(!engine.step_depends_on(&step_with_number_template, "step2"));
        
    //     // Test case 20: Step with boolean template (should not match)
    //     let step_with_boolean_template = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "boolean".to_string(),
    //                 template: Value::Bool(true),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         priority: 0,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(!engine.step_depends_on(&step_with_boolean_template, "step2"));
        
    //     // Test case 21: Step with null template (should not match)
    //     let step_with_null_template = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "null".to_string(),
    //                 template: Value::Null,
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         parent_action: None,
    //         priority: 0,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(!engine.step_depends_on(&step_with_null_template, "step2"));
        
    //     // Test case 22: Step with mixed types including numbers and booleans (should only match strings)
    //     let step_with_mixed_types = ShAction {
    //         id: "step1".to_string(),
    //         name: "test_step".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test/action:1.0.0".to_string(),
    //         priority: 0,
    //         inputs: vec![
    //             ShIO {
    //                 name: "input1".to_string(),
    //                 r#type: "object".to_string(),
    //                 template: Value::Object({
    //                     let mut map = serde_json::Map::new();
    //                     map.insert("string_field".to_string(), Value::String("{{steps.step17.result}}".to_string()));
    //                     map.insert("number_field".to_string(), Value::Number(serde_json::Number::from(42)));
    //                     map.insert("boolean_field".to_string(), Value::Bool(true));
    //                     map.insert("null_field".to_string(), Value::Null);
    //                     map.insert("array_field".to_string(), Value::Array(vec![
    //                         Value::String("{{steps.step18.data}}".to_string()),
    //                         Value::Number(serde_json::Number::from(100)),
    //                         Value::Bool(false)
    //                     ]));
    //                     map
    //                 }),
    //                 value: None,
    //                 required: true,
    //             }
    //         ],
    //         outputs: vec![],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         role: None,
    //         types: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
        
    //     assert!(engine.step_depends_on(&step_with_mixed_types, "step17"));
    //     assert!(engine.step_depends_on(&step_with_mixed_types, "step18"));
    //     assert!(!engine.step_depends_on(&step_with_mixed_types, "step2"));
    // }

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

    // #[test]
    // fn test_interpolate_string() {
    //     let engine = ExecutionEngine::new();
        
    //     // Test case 1: Simple input interpolation without jsonpath
    //     let template1 = "Hello {{inputs[0]}} world!";
    //     let variables1 = vec![Value::String("John".to_string())];
    //     let _executed_steps1: HashMap<String, ShAction> = HashMap::new();
    //     let result1 = engine.interpolate_string_from_parent_input(template1, &variables1).unwrap();
    //     assert_eq!(result1, "Hello John world!");
        
    //     // Test case 2: Multiple input interpolations
    //     let template2 = "{{inputs[0]}} and {{inputs[1]}} are friends";
    //     let variables2 = vec![
    //         Value::String("Alice".to_string()),
    //         Value::String("Bob".to_string())
    //     ];
    //     let _executed_steps2: HashMap<String, ShAction> = HashMap::new();
    //     let result2 = engine.interpolate_string_from_parent_input(template2, &variables2).unwrap();
    //     assert_eq!(result2, "Alice and Bob are friends");
        
    //     // Test case 3: Input interpolation with non-string values
    //     let template3 = "The number is {{inputs[0]}}";
    //     let variables3 = vec![Value::Number(serde_json::Number::from(42))];
    //     let _executed_steps3: HashMap<String, ShAction> = HashMap::new();
    //     let result3 = engine.interpolate_string_from_parent_input(template3, &variables3).unwrap();
    //     assert_eq!(result3, "The number is 42");
        
    //     // Test case 4: Input interpolation with boolean values
    //     let template4 = "Status: {{inputs[0]}}";
    //     let variables4 = vec![Value::Bool(true)];
    //     let _executed_steps4: HashMap<String, ShAction> = HashMap::new();
    //     let result4 = engine.interpolate_string_from_parent_input(template4, &variables4).unwrap();
    //     assert_eq!(result4, "Status: true");
        
    //     // Test case 5: Input interpolation with null values
    //     let template5 = "Value: {{inputs[0]}}";
    //     let variables5 = vec![Value::Null];
    //     let _executed_steps5: HashMap<String, ShAction> = HashMap::new();
    //     let result5 = engine.interpolate_string_from_parent_input(template5, &variables5).unwrap();
    //     assert_eq!(result5, "Value: null");
        
    //     // Test case 6: Input interpolation with JSONPath
    //     let template6 = "Name: {{inputs[0].name}}";
    //     let variables6 = vec![Value::Object({
    //         let mut map = serde_json::Map::new();
    //         map.insert("name".to_string(), Value::String("Charlie".to_string()));
    //         map.insert("age".to_string(), Value::Number(serde_json::Number::from(30)));
    //         map
    //     })];
    //     let _executed_steps6: HashMap<String, ShAction> = HashMap::new();
    //     let result6 = engine.interpolate_string_from_parent_input(template6, &variables6).unwrap();
    //     assert_eq!(result6, "Name: Charlie");
        
    //     // Test case 7: Input interpolation with nested JSONPath
    //     let template7 = "City: {{inputs[0].address.city}}";
    //     let variables7 = vec![Value::Object({
    //         let mut map = serde_json::Map::new();
    //         map.insert("name".to_string(), Value::String("David".to_string()));
    //         map.insert("address".to_string(), Value::Object({
    //             let mut addr_map = serde_json::Map::new();
    //             addr_map.insert("city".to_string(), Value::String("New York".to_string()));
    //             addr_map.insert("country".to_string(), Value::String("USA".to_string()));
    //             addr_map
    //         }));
    //         map
    //     })];
    //     let _executed_steps7: HashMap<String, ShAction> = HashMap::new();
    //     let result7 = engine.interpolate_string_from_parent_input(template7, &variables7).unwrap();
    //     assert_eq!(result7, "City: New York");
        
    //     // Test case 8: Input interpolation with array access
    //     let template8 = "First item: {{inputs[0].items.0}}";
    //     let variables8 = vec![Value::Object({
    //         let mut map = serde_json::Map::new();
    //         map.insert("items".to_string(), Value::Array(vec![
    //             Value::String("apple".to_string()),
    //             Value::String("banana".to_string())
    //         ]));
    //         map
    //     })];
    //     let _executed_steps8: HashMap<String, ShAction> = HashMap::new();
    //     let result8 = engine.interpolate_string_from_parent_input(template8, &variables8).unwrap();
    //     assert_eq!(result8, "First item: apple");
        
    //     // Test case 9: Step output interpolation without jsonpath
    //     let template9 = "Result: {{steps.step1.outputs[0]}}";
    //     let executed_steps9 = {
    //         let mut map = HashMap::new();
    //         let step1 = ShAction {
    //             id: "step1".to_string(),
    //             name: "test_step".to_string(),
    //             kind: "wasm".to_string(),
    //             uses: "test/action:1.0.0".to_string(),
    //             inputs: vec![],
    //             outputs: vec![
    //                 ShIO {
    //                     name: "result".to_string(),
    //                     r#type: "string".to_string(),
    //                     template: Value::String("test_result".to_string()),
    //                     value: Some(Value::String("Hello from step1".to_string())),
    //                     required: true,
    //                 }
    //             ],
    //             parent_action: None,
    //             steps: HashMap::new(),
    //             role: None,
    //             types: None,
    //             mirrors: vec![],
    //             permissions: None,
    //         };
    //         map.insert("step1".to_string(), step1);
    //         map
    //     };
    //     let result9 = engine.interpolate_string_from_sibling_output(template9, &executed_steps9).unwrap();
    //     assert_eq!(result9, Value::String("Result: Hello from step1".to_string()));
        
    //     // Test case 10: Step output interpolation with jsonpath
    //     let template10 = "Name: {{steps.step2.outputs[0].name}}";
    //     let executed_steps10 = {
    //         let mut map = HashMap::new();
    //         let step2 = ShAction {
    //             id: "step2".to_string(),
    //             name: "test_step2".to_string(),
    //             kind: "wasm".to_string(),
    //             uses: "test/action:1.0.0".to_string(),
    //             inputs: vec![],
    //             outputs: vec![
    //                 ShIO {
    //                     name: "data".to_string(),
    //                     r#type: "object".to_string(),
    //                     template: Value::String("test_data".to_string()),
    //                     value: Some(Value::Object({
    //                         let mut data_map = serde_json::Map::new();
    //                         data_map.insert("name".to_string(), Value::String("Eve".to_string()));
    //                         data_map.insert("age".to_string(), Value::Number(serde_json::Number::from(25)));
    //                         data_map
    //                     })),
    //                     required: true,
    //                 }
    //             ],
    //             parent_action: None,
    //             steps: HashMap::new(),
    //             role: None,
    //             types: None,
    //             mirrors: vec![],
    //             permissions: None,
    //         };
    //         map.insert("step2".to_string(), step2);
    //         map
    //     };
    //     let result10 = engine.interpolate_string_from_sibling_output(template10, &executed_steps10).unwrap();
    //     assert_eq!(result10, Value::String("Name: Eve".to_string()));
        
    //     // Test case 11: Mixed input and step interpolation
    //     let template11 = "{{inputs[0]}} used {{steps.step3.outputs[0]}}";
    //     let variables11 = vec![Value::String("Frank".to_string())];
    //     let executed_steps11 = {
    //         let mut map = HashMap::new();
    //         let step3 = ShAction {
    //             id: "step3".to_string(),
    //             name: "test_step3".to_string(),
    //             kind: "wasm".to_string(),
    //             uses: "test/action:1.0.0".to_string(),
    //             inputs: vec![],
    //             outputs: vec![
    //                 ShIO {
    //                     name: "tool".to_string(),
    //                     r#type: "string".to_string(),
    //                     template: Value::String("test_tool".to_string()),
    //                     value: Some(Value::String("hammer".to_string())),
    //                     required: true,
    //                 }
    //             ],
    //             parent_action: None,
    //             steps: HashMap::new(),
    //             role: None,
    //             types: None,
    //             mirrors: vec![],
    //             permissions: None,
    //         };
    //         map.insert("step3".to_string(), step3);
    //         map
    //     };
    //     let result11 = engine.interpolate_string_from_parent_input(template11, &variables11).unwrap();
    //     let result11 = match result11 {
    //         Value::String(s) => engine.interpolate_string_from_sibling_output(&s, &executed_steps11).unwrap(),
    //         _ => result11,
    //     };
    //     assert_eq!(result11, Value::String("Frank used hammer".to_string()));
        
    //     // Test case 12: Template with no interpolation (should return unchanged)
    //     let template12 = "Hello world!";
    //     let variables12 = vec![];
    //     let _executed_steps12: HashMap<String, ShAction> = HashMap::new();
    //     let result12 = engine.interpolate_string_from_parent_input(template12, &variables12).unwrap();
    //     assert_eq!(result12, "Hello world!");
        
    //     // Test case 13: Template with malformed interpolation (should leave unchanged)
    //     let template13 = "Hello {{inputs[0 world!";
    //     let variables13 = vec![Value::String("John".to_string())];
    //     let _executed_steps13: HashMap<String, ShAction> = HashMap::new();
    //     let result13 = engine.interpolate_string_from_parent_input(template13, &variables13).unwrap();
    //     assert_eq!(result13, "Hello {{inputs[0 world!");
        
    //     // Test case 14: Input index out of bounds (should leave unchanged)
    //     let template14 = "Hello {{inputs[5]}} world!";
    //     let variables14 = vec![Value::String("John".to_string())];
    //     let _executed_steps14: HashMap<String, ShAction> = HashMap::new();
    //     let result14 = engine.interpolate_string_from_parent_input(template14, &variables14).unwrap();
    //     assert_eq!(result14, "Hello {{inputs[5]}} world!");
        
    //     // Test case 15: Step not found (should leave unchanged)
    //     let template15 = "Result: {{steps.nonexistent.outputs[0]}}";
    //     let executed_steps15 = HashMap::new();
    //     let result15 = engine.interpolate_string_from_sibling_output(template15, &executed_steps15).unwrap();
    //     assert_eq!(result15, Value::String("Result: {{steps.nonexistent.outputs[0]}}".to_string()));
        
    //     // Test case 16: Step output index out of bounds (should leave unchanged)
    //     let template16 = "Result: {{steps.step1.outputs[5]}}";
    //     let executed_steps16 = {
    //         let mut map = HashMap::new();
    //         let step1 = ShAction {
    //             id: "step1".to_string(),
    //             name: "test_step".to_string(),
    //             kind: "wasm".to_string(),
    //             uses: "test/action:1.0.0".to_string(),
    //             inputs: vec![],
    //             outputs: vec![
    //                 ShIO {
    //                     name: "result".to_string(),
    //                     r#type: "string".to_string(),
    //                     template: Value::String("test_result".to_string()),
    //                     value: Some(Value::String("Hello".to_string())),
    //                     required: true,
    //                 }
    //             ],
    //             parent_action: None,
    //             steps: HashMap::new(),
    //             role: None,
    //             types: None,
    //             mirrors: vec![],
    //             permissions: None,
    //         };
    //         map.insert("step1".to_string(), step1);
    //         map
    //     };
    //     let result16 = engine.interpolate_string_from_sibling_output(template16, &executed_steps16).unwrap();
    //     assert_eq!(result16, Value::String("Result: {{steps.step1.outputs[5]}}".to_string()));
        
    //     // Test case 17: Step output with no value (should leave unchanged)
    //     let template17 = "Result: {{steps.step4.outputs[0]}}";
    //     let executed_steps17 = {
    //         let mut map = HashMap::new();
    //         let step4 = ShAction {
    //             id: "step4".to_string(),
    //             name: "test_step4".to_string(),
    //             kind: "wasm".to_string(),
    //             uses: "test/action:1.0.0".to_string(),
    //             inputs: vec![],
    //             outputs: vec![
    //                 ShIO {
    //                     name: "result".to_string(),
    //                     r#type: "string".to_string(),
    //                     template: Value::String("test_result".to_string()),
    //                     value: None, // No value
    //                     required: true,
    //                 }
    //             ],
    //             parent_action: None,
    //             steps: HashMap::new(),
    //             role: None,
    //             types: None,
    //             mirrors: vec![],
    //             permissions: None,
    //         };
    //         map.insert("step4".to_string(), step4);
    //         map
    //     };
    //     let result17 = engine.interpolate_string_from_sibling_output(template17, &executed_steps17).unwrap();
    //     assert_eq!(result17, Value::String("Result: {{steps.step4.outputs[0]}}".to_string()));
        
    //     // Test case 18: Complex nested JSONPath
    //     let template18 = "User: {{inputs[0].user.profile.name}}";
    //     let variables18 = vec![Value::Object({
    //         let mut map = serde_json::Map::new();
    //         map.insert("user".to_string(), Value::Object({
    //             let mut user_map = serde_json::Map::new();
    //             user_map.insert("profile".to_string(), Value::Object({
    //                 let mut profile_map = serde_json::Map::new();
    //                 profile_map.insert("name".to_string(), Value::String("Grace".to_string()));
    //                 profile_map.insert("email".to_string(), Value::String("grace@example.com".to_string()));
    //                 profile_map
    //             }));
    //             user_map
    //         }));
    //         map
    //     })];
    //     let _executed_steps18: HashMap<String, ShAction> = HashMap::new();
    //     let result18 = engine.interpolate_string_from_parent_input(template18, &variables18).unwrap();
    //     assert_eq!(result18, "User: Grace");
        
    //     // Test case 19: Multiple interpolations of same pattern
    //     let template19 = "{{inputs[0]}} and {{inputs[0]}} are the same";
    //     let variables19 = vec![Value::String("Henry".to_string())];
    //     let result19 = engine.interpolate_string_from_parent_input(template19, &variables19).unwrap();
    //     assert_eq!(result19, "Henry and Henry are the same");
        
    //     // Test case 20: Empty template
    //     let template20 = "";
    //     let variables20 = vec![];
    //     let result20 = engine.interpolate_string_from_parent_input(template20, &variables20).unwrap();
    //     assert_eq!(result20, "");
    // }

    // #[test]
    // fn test_interpolate_string_from_parent_input_or_sibling_output() {
    //     let engine = ExecutionEngine::new();
        
    //     // Test case 1: Only parent inputs (no sibling outputs)
    //     let template1 = "Hello {{inputs[0]}} world!";
    //     let variables1 = vec![Value::String("John".to_string())];
    //     let _executed_steps1: HashMap<String, ShAction> = HashMap::new();
    //     let result1 = engine.interpolate_string_from_parent_input_or_sibling_output(template1, &variables1, &_executed_steps1).unwrap();
    //     assert_eq!(result1, Value::String("Hello John world!".to_string()));
        
    //     // Test case 2: Only sibling outputs (no parent inputs)
    //     let template2 = "Result from {{steps.step1.outputs[0]}}";
    //     let variables2 = vec![];
    //     let mut executed_steps2: HashMap<String, ShAction> = HashMap::new();
    //     let step1 = ShAction {
    //         id: "step1".to_string(),
    //         name: "step1".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test-action".to_string(),
    //         inputs: vec![],
    //         outputs: vec![ShIO {
    //             name: "output0".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("test_result".to_string()),
    //             value: Some(Value::String("test_result".to_string())),
    //             required: false,
    //         }],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         types: None,
    //         role: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
    //     executed_steps2.insert("step1".to_string(), step1);
    //     let result2 = engine.interpolate_string_from_parent_input_or_sibling_output(template2, &variables2, &executed_steps2).unwrap();
    //     assert_eq!(result2, Value::String("Result from test_result".to_string()));
        
    //     // Test case 3: Mixed parent inputs and sibling outputs
    //     let template3 = "Hello {{inputs[0]}} from {{steps.step1.outputs[0]}}";
    //     let variables3 = vec![Value::String("John".to_string())];
    //     let mut executed_steps3: HashMap<String, ShAction> = HashMap::new();
    //     let step1_3 = ShAction {
    //         id: "step1".to_string(),
    //         name: "step1".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test-action".to_string(),
    //         inputs: vec![],
    //         outputs: vec![ShIO {
    //             name: "output0".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("step1_result".to_string()),
    //             value: Some(Value::String("step1_result".to_string())),
    //             required: false,
    //         }],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         types: None,
    //         role: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
    //     executed_steps3.insert("step1".to_string(), step1_3);
    //     let result3 = engine.interpolate_string_from_parent_input_or_sibling_output(template3, &variables3, &executed_steps3).unwrap();
    //     assert_eq!(result3, Value::String("Hello John from step1_result".to_string()));
        
    //     // Test case 4: Multiple parent inputs and sibling outputs
    //     let template4 = "{{inputs[0]}} and {{inputs[1]}} from {{steps.step1.outputs[0]}} and {{steps.step2.outputs[0]}}";
    //     let variables4 = vec![
    //         Value::String("Alice".to_string()),
    //         Value::String("Bob".to_string())
    //     ];
    //     let mut executed_steps4: HashMap<String, ShAction> = HashMap::new();
    //     let step1_4 = ShAction {
    //         id: "step1".to_string(),
    //         name: "step1".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test-action".to_string(),
    //         inputs: vec![],
    //         outputs: vec![ShIO {
    //             name: "output0".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("step1_result".to_string()),
    //             value: Some(Value::String("step1_result".to_string())),
    //             required: false,
    //         }],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         types: None,
    //         role: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
    //     let step2_4 = ShAction {
    //         id: "step2".to_string(),
    //         name: "step2".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test-action".to_string(),
    //         inputs: vec![],
    //         outputs: vec![ShIO {
    //             name: "output0".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("step2_result".to_string()),
    //             value: Some(Value::String("step2_result".to_string())),
    //             required: false,
    //         }],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         types: None,
    //         role: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
    //     executed_steps4.insert("step1".to_string(), step1_4);
    //     executed_steps4.insert("step2".to_string(), step2_4);
    //     let result4 = engine.interpolate_string_from_parent_input_or_sibling_output(template4, &variables4, &executed_steps4).unwrap();
    //     assert_eq!(result4, Value::String("Alice and Bob from step1_result and step2_result".to_string()));
        
    //     // Test case 5: Parent inputs with JSONPath and sibling outputs
    //     let template5 = "Name: {{inputs[0].name}} from {{steps.step1.outputs[0]}}";
    //     let variables5 = vec![Value::Object({
    //         let mut map = serde_json::Map::new();
    //         map.insert("name".to_string(), Value::String("Charlie".to_string()));
    //         map
    //     })];
    //     let mut executed_steps5: HashMap<String, ShAction> = HashMap::new();
    //     let step1_5 = ShAction {
    //         id: "step1".to_string(),
    //         name: "step1".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test-action".to_string(),
    //         inputs: vec![],
    //         outputs: vec![ShIO {
    //             name: "output0".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("step1_result".to_string()),
    //             value: Some(Value::String("step1_result".to_string())),
    //             required: false,
    //         }],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         types: None,
    //         role: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
    //     executed_steps5.insert("step1".to_string(), step1_5);
    //     let result5 = engine.interpolate_string_from_parent_input_or_sibling_output(template5, &variables5, &executed_steps5).unwrap();
    //     assert_eq!(result5, Value::String("Name: Charlie from step1_result".to_string()));
        
    //     // Test case 6: Sibling outputs with JSONPath
    //     let template6 = "{{inputs[0]}} from {{steps.step1.outputs[0].data}}";
    //     let variables6 = vec![Value::String("Input".to_string())];
    //     let mut executed_steps6: HashMap<String, ShAction> = HashMap::new();
    //     let step1_6 = ShAction {
    //         id: "step1".to_string(),
    //         name: "step1".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test-action".to_string(),
    //         inputs: vec![],
    //         outputs: vec![ShIO {
    //             name: "output0".to_string(),
    //             r#type: "object".to_string(),
    //             template: Value::Object({
    //                 let mut map = serde_json::Map::new();
    //                 map.insert("data".to_string(), Value::String("step1_data".to_string()));
    //                 map
    //             }),
    //             value: Some(Value::Object({
    //                 let mut map = serde_json::Map::new();
    //                 map.insert("data".to_string(), Value::String("step1_data".to_string()));
    //                 map
    //             })),
    //             required: false,
    //         }],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         types: None,
    //         role: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
    //     executed_steps6.insert("step1".to_string(), step1_6);
    //     let result6 = engine.interpolate_string_from_parent_input_or_sibling_output(template6, &variables6, &executed_steps6).unwrap();
    //     assert_eq!(result6, Value::String("Input from step1_data".to_string()));
        
    //     // Test case 7: Non-string values from both sources
    //     let template7 = "Number {{inputs[0]}} and result {{steps.step1.outputs[0]}}";
    //     let variables7 = vec![Value::Number(serde_json::Number::from(42))];
    //     let mut executed_steps7: HashMap<String, ShAction> = HashMap::new();
    //     let step1_7 = ShAction {
    //         id: "step1".to_string(),
    //         name: "step1".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test-action".to_string(),
    //         inputs: vec![],
    //         outputs: vec![ShIO {
    //             name: "output0".to_string(),
    //             r#type: "number".to_string(),
    //             template: Value::Number(serde_json::Number::from(100)),
    //             value: Some(Value::Number(serde_json::Number::from(100))),
    //             required: false,
    //         }],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         types: None,
    //         role: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
    //     executed_steps7.insert("step1".to_string(), step1_7);
    //     let result7 = engine.interpolate_string_from_parent_input_or_sibling_output(template7, &variables7, &executed_steps7).unwrap();
    //     assert_eq!(result7, Value::String("Number 42 and result 100".to_string()));
        
    //     // Test case 8: Boolean values from both sources
    //     let template8 = "Status {{inputs[0]}} and flag {{steps.step1.outputs[0]}}";
    //     let variables8 = vec![Value::Bool(true)];
    //     let mut executed_steps8: HashMap<String, ShAction> = HashMap::new();
    //     let step1_8 = ShAction {
    //         id: "step1".to_string(),
    //         name: "step1".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test-action".to_string(),
    //         inputs: vec![],
    //         outputs: vec![ShIO {
    //             name: "output0".to_string(),
    //             r#type: "boolean".to_string(),
    //             template: Value::Bool(false),
    //             value: Some(Value::Bool(false)),
    //             required: false,
    //         }],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         types: None,
    //         role: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
    //     executed_steps8.insert("step1".to_string(), step1_8);
    //     let result8 = engine.interpolate_string_from_parent_input_or_sibling_output(template8, &variables8, &executed_steps8).unwrap();
    //     assert_eq!(result8, Value::String("Status true and flag false".to_string()));
        
    //     // Test case 9: Complex mixed template with multiple references
    //     let template9 = "{{inputs[0]}} {{inputs[1]}} from {{steps.step1.outputs[0]}} and {{steps.step2.outputs[0]}} with {{inputs[0].name}}";
    //     let variables9 = vec![
    //         Value::Object({
    //             let mut map = serde_json::Map::new();
    //             map.insert("name".to_string(), Value::String("Alice".to_string()));
    //             map
    //         }),
    //         Value::String("Bob".to_string())
    //     ];
    //     let mut executed_steps9: HashMap<String, ShAction> = HashMap::new();
    //     let step1_9 = ShAction {
    //         id: "step1".to_string(),
    //         name: "step1".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test-action".to_string(),
    //         inputs: vec![],
    //         outputs: vec![ShIO {
    //             name: "output0".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("step1_result".to_string()),
    //             value: Some(Value::String("step1_result".to_string())),
    //             required: false,
    //         }],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         types: None,
    //         role: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
    //     let step2_9 = ShAction {
    //         id: "step2".to_string(),
    //         name: "step2".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "test-action".to_string(),
    //         inputs: vec![],
    //         outputs: vec![ShIO {
    //             name: "output0".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("step2_result".to_string()),
    //             value: Some(Value::String("step2_result".to_string())),
    //             required: false,
    //         }],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         types: None,
    //         role: None,
    //         mirrors: vec![],
    //         permissions: None,
    //     };
    //     executed_steps9.insert("step1".to_string(), step1_9);
    //     executed_steps9.insert("step2".to_string(), step2_9);
    //     let result9 = engine.interpolate_string_from_parent_input_or_sibling_output(template9, &variables9, &executed_steps9).unwrap();
    //     assert_eq!(result9, Value::String("{\"name\":\"Alice\"} Bob from step1_result and step2_result with Alice".to_string()));
        
    //     // Test case 10: Empty template
    //     let template10 = "";
    //     let variables10 = vec![Value::String("test".to_string())];
    //     let _executed_steps10: HashMap<String, ShAction> = HashMap::new();
    //     let result10 = engine.interpolate_string_from_parent_input_or_sibling_output(template10, &variables10, &_executed_steps10).unwrap();
    //     assert_eq!(result10, Value::String("".to_string()));
    // }

    // #[test]
    // fn test_interpolate() {
    //     let engine = ExecutionEngine::new();
        
    //     // Test case 1: String template with simple interpolation
    //     let template1 = Value::String("Hello {{inputs[0]}} world!".to_string());
    //     let variables1 = vec![Value::String("John".to_string())];
    //     let _executed_steps1: HashMap<String, ShAction> = HashMap::new();
    //     let result1 = engine.interpolate_from_parent_inputs(&template1, &variables1).unwrap();
    //     assert_eq!(result1, Value::String("Hello John world!".to_string()));
        
    //     // Test case 2: String template that resolves to JSON object
    //     let template2 = Value::String("{\"name\": \"{{inputs[0]}}\", \"age\": {{inputs[1]}}}".to_string());
    //     let variables2 = vec![
    //         Value::String("Alice".to_string()),
    //         Value::Number(serde_json::Number::from(25))
    //     ];
    //     let _executed_steps2: HashMap<String, ShAction> = HashMap::new();
    //     let result2 = engine.interpolate_from_parent_inputs(&template2, &variables2).unwrap();
    //     let expected2 = Value::Object({
    //         let mut map = serde_json::Map::new();
    //         map.insert("name".to_string(), Value::String("Alice".to_string()));
    //         map.insert("age".to_string(), Value::Number(serde_json::Number::from(25)));
    //         map
    //     });
    //     assert_eq!(result2, expected2);
        
    //     // Test case 3: String template that resolves to JSON array
    //     let template3 = Value::String("[\"{{inputs[0]}}\", \"{{inputs[1]}}\", \"{{inputs[2]}}\"]".to_string());
    //     let variables3 = vec![
    //         Value::String("apple".to_string()),
    //         Value::String("banana".to_string()),
    //         Value::String("cherry".to_string())
    //     ];
    //     let _executed_steps3: HashMap<String, ShAction> = HashMap::new();
    //     let result3 = engine.interpolate_from_parent_inputs(&template3, &variables3).unwrap();
    //     let expected3 = Value::Array(vec![
    //         Value::String("apple".to_string()),
    //         Value::String("banana".to_string()),
    //         Value::String("cherry".to_string())
    //     ]);
    //     assert_eq!(result3, expected3);
        
    //     // Test case 4: String template that resolves to JSON primitive (number)
    //     let template4 = Value::String("{{inputs[0]}}".to_string());
    //     let variables4 = vec![Value::Number(serde_json::Number::from(42))];
    //     let _executed_steps4: HashMap<String, ShAction> = HashMap::new();
    //     let result4 = engine.interpolate_from_parent_inputs(&template4, &variables4).unwrap();
    //     assert_eq!(result4, Value::Number(serde_json::Number::from(42)));
        
    //     // Test case 5: String template that resolves to JSON primitive (boolean)
    //     let template5 = Value::String("{{inputs[0]}}".to_string());
    //     let variables5 = vec![Value::Bool(true)];
    //     let _executed_steps5: HashMap<String, ShAction> = HashMap::new();
    //     let result5 = engine.interpolate_from_parent_inputs(&template5, &variables5).unwrap();
    //     assert_eq!(result5, Value::Bool(true));
        
    //     // Test case 6: String template that resolves to JSON primitive (null)
    //     let template6 = Value::String("{{inputs[0]}}".to_string());
    //     let variables6 = vec![Value::Null];
    //     let _executed_steps6: HashMap<String, ShAction> = HashMap::new();
    //     let result6 = engine.interpolate_from_parent_inputs(&template6, &variables6).unwrap();
    //     assert_eq!(result6, Value::Null);
        
    //     // Test case 7: String template that doesn't resolve to valid JSON
    //     let template7 = Value::String("Hello {{inputs[0]}} world!".to_string());
    //     let variables7 = vec![Value::String("John".to_string())];
    //     let _executed_steps7: HashMap<String, ShAction> = HashMap::new();
    //     let result7 = engine.interpolate_from_parent_inputs(&template7, &variables7).unwrap();
    //     assert_eq!(result7, Value::String("Hello John world!".to_string()));
        
    //     // Test case 8: Object template with string interpolation
    //     let template8 = Value::Object({
    //         let mut map = serde_json::Map::new();
    //         map.insert("name".to_string(), Value::String("{{inputs[0]}}".to_string()));
    //         map.insert("age".to_string(), Value::String("{{inputs[1]}}".to_string()));
    //         map.insert("city".to_string(), Value::String("New York".to_string()));
    //         map
    //     });
    //     let variables8 = vec![
    //         Value::String("Bob".to_string()),
    //         Value::Number(serde_json::Number::from(30))
    //     ];
    //     let _executed_steps8: HashMap<String, ShAction> = HashMap::new();
    //     let result8 = engine.interpolate_from_parent_inputs(&template8, &variables8).unwrap();
    //     let expected8 = Value::Object({
    //         let mut map = serde_json::Map::new();
    //         map.insert("name".to_string(), Value::String("Bob".to_string()));
    //         map.insert("age".to_string(), Value::Number(serde_json::Number::from(30)));
    //         map.insert("city".to_string(), Value::String("New York".to_string()));
    //         map
    //     });
    //     assert_eq!(result8, expected8);
        
    //     // Test case 9: Object template with nested object interpolation
    //     let template9 = Value::Object({
    //         let mut map = serde_json::Map::new();
    //         map.insert("user".to_string(), Value::Object({
    //             let mut user_map = serde_json::Map::new();
    //             user_map.insert("name".to_string(), Value::String("{{inputs[0]}}".to_string()));
    //             user_map.insert("profile".to_string(), Value::Object({
    //                 let mut profile_map = serde_json::Map::new();
    //                 profile_map.insert("email".to_string(), Value::String("{{inputs[1]}}".to_string()));
    //                 profile_map
    //             }));
    //             user_map
    //         }));
    //         map
    //     });
    //     let variables9 = vec![
    //         Value::String("Charlie".to_string()),
    //         Value::String("charlie@example.com".to_string())
    //     ];
    //     let _executed_steps9: HashMap<String, ShAction> = HashMap::new();
    //     let result9 = engine.interpolate_from_parent_inputs(&template9, &variables9).unwrap();
    //     let expected9 = Value::Object({
    //         let mut map = serde_json::Map::new();
    //         map.insert("user".to_string(), Value::Object({
    //             let mut user_map = serde_json::Map::new();
    //             user_map.insert("name".to_string(), Value::String("Charlie".to_string()));
    //             user_map.insert("profile".to_string(), Value::Object({
    //                 let mut profile_map = serde_json::Map::new();
    //                 profile_map.insert("email".to_string(), Value::String("charlie@example.com".to_string()));
    //                 profile_map
    //             }));
    //             user_map
    //         }));
    //         map
    //     });
    //     assert_eq!(result9, expected9);
        
    //     // Test case 10: Array template with string interpolation
    //     let template10 = Value::Array(vec![
    //         Value::String("{{inputs[0]}}".to_string()),
    //         Value::String("{{inputs[1]}}".to_string()),
    //         Value::String("{{inputs[2]}}".to_string())
    //     ]);
    //     let variables10 = vec![
    //         Value::String("red".to_string()),
    //         Value::String("green".to_string()),
    //         Value::String("blue".to_string())
    //     ];
    //     let _executed_steps10: HashMap<String, ShAction> = HashMap::new();
    //     let result10 = engine.interpolate_from_parent_inputs(&template10, &variables10).unwrap();
    //     let expected10 = Value::Array(vec![
    //         Value::String("red".to_string()),
    //         Value::String("green".to_string()),
    //         Value::String("blue".to_string())
    //     ]);
    //     assert_eq!(result10, expected10);
        
    //     // Test case 11: Array template with mixed types
    //     let template11 = Value::Array(vec![
    //         Value::String("{{inputs[0]}}".to_string()),
    //         Value::Number(serde_json::Number::from(42)),
    //         Value::Bool(true),
    //         Value::Null
    //     ]);
    //     let variables11 = vec![Value::String("test".to_string())];
    //     let _executed_steps11: HashMap<String, ShAction> = HashMap::new();
    //     let result11 = engine.interpolate_from_parent_inputs(&template11, &variables11).unwrap();
    //     let expected11 = Value::Array(vec![
    //         Value::String("test".to_string()),
    //         Value::Number(serde_json::Number::from(42)),
    //         Value::Bool(true),
    //         Value::Null
    //     ]);
    //     assert_eq!(result11, expected11);
        
    //     // Test case 12: Array template with nested arrays
    //     let template12 = Value::Array(vec![
    //         Value::Array(vec![
    //             Value::String("{{inputs[0]}}".to_string()),
    //             Value::String("{{inputs[1]}}".to_string())
    //         ]),
    //         Value::Array(vec![
    //             Value::String("{{inputs[2]}}".to_string()),
    //             Value::String("{{inputs[3]}}".to_string())
    //         ])
    //     ]);
    //     let variables12 = vec![
    //         Value::String("a".to_string()),
    //         Value::String("b".to_string()),
    //         Value::String("c".to_string()),
    //         Value::String("d".to_string())
    //     ];
    //     let _executed_steps12: HashMap<String, ShAction> = HashMap::new();
    //     let result12 = engine.interpolate_from_parent_inputs(&template12, &variables12).unwrap();
    //     let expected12 = Value::Array(vec![
    //         Value::Array(vec![
    //             Value::String("a".to_string()),
    //             Value::String("b".to_string())
    //         ]),
    //         Value::Array(vec![
    //             Value::String("c".to_string()),
    //             Value::String("d".to_string())
    //         ])
    //     ]);
    //     assert_eq!(result12, expected12);
        
    //     // Test case 13: Complex nested structure with step interpolation
    //     let template13 = Value::Object({
    //         let mut map = serde_json::Map::new();
    //         map.insert("user".to_string(), Value::String("{{inputs[0]}}".to_string()));
    //         map.insert("data".to_string(), Value::Array(vec![
    //             Value::String("{{steps.step1.outputs[0]}}".to_string()),
    //             Value::String("{{steps.step2.outputs[0]}}".to_string())
    //         ]));
    //         map
    //     });
    //     let variables13 = vec![Value::String("David".to_string())];
    //     let executed_steps13 = {
    //         let mut map = HashMap::new();
    //         let step1 = ShAction {
    //             id: "step1".to_string(),
    //             name: "test_step1".to_string(),
    //             kind: "wasm".to_string(),
    //             uses: "test/action:1.0.0".to_string(),
    //             inputs: vec![],
    //             outputs: vec![
    //                 ShIO {
    //                     name: "result1".to_string(),
    //                     r#type: "string".to_string(),
    //                     template: Value::String("test_result1".to_string()),
    //                     value: Some(Value::String("Hello from step1".to_string())),
    //                     required: true,
    //                 }
    //             ],
    //             parent_action: None,
    //             steps: HashMap::new(),
    //             role: None,
    //             types: None,
    //             mirrors: vec![],
    //             permissions: None,
    //         };
    //         let step2 = ShAction {
    //             id: "step2".to_string(),
    //             name: "test_step2".to_string(),
    //             kind: "wasm".to_string(),
    //             uses: "test/action:1.0.0".to_string(),
    //             inputs: vec![],
    //             outputs: vec![
    //                 ShIO {
    //                     name: "result2".to_string(),
    //                     r#type: "string".to_string(),
    //                     template: Value::String("test_result2".to_string()),
    //                     value: Some(Value::String("Hello from step2".to_string())),
    //                     required: true,
    //                 }
    //             ],
    //             parent_action: None,
    //             steps: HashMap::new(),
    //             role: None,
    //             types: None,
    //             mirrors: vec![],
    //             permissions: None,
    //         };
    //         map.insert("step1".to_string(), step1);
    //         map.insert("step2".to_string(), step2);
    //         map
    //     };
    //     let result13 = engine.interpolate_recursively_from_parent_input_or_sibling_output(&template13, &variables13, &executed_steps13).unwrap();
    //     let expected13 = Value::Object({
    //         let mut map = serde_json::Map::new();
    //         map.insert("user".to_string(), Value::String("David".to_string()));
    //         map.insert("data".to_string(), Value::Array(vec![
    //             Value::String("Hello from step1".to_string()),
    //             Value::String("Hello from step2".to_string())
    //         ]));
    //         map
    //     });
    //     assert_eq!(result13, expected13);
        
    //     // Test case 14: Primitive values (should return unchanged)
    //     let template14 = Value::Number(serde_json::Number::from(123));
    //     let variables14 = vec![];
    //     let result14 = engine.interpolate_from_parent_inputs(&template14, &variables14).unwrap();
    //     assert_eq!(result14, Value::Number(serde_json::Number::from(123)));
        
    //     // Test case 15: Boolean primitive (should return unchanged)
    //     let template15 = Value::Bool(false);
    //     let variables15 = vec![];
    //     let _executed_steps15: HashMap<String, ShAction> = HashMap::new();
    //     let result15 = engine.interpolate_from_parent_inputs(&template15, &variables15).unwrap();
    //     assert_eq!(result15, Value::Bool(false));
        
    //     // Test case 16: Null primitive (should return unchanged)
    //     let template16 = Value::Null;
    //     let variables16 = vec![];
    //     let _executed_steps16: HashMap<String, ShAction> = HashMap::new();
    //     let result16 = engine.interpolate_from_parent_inputs(&template16, &variables16).unwrap();
    //     assert_eq!(result16, Value::Null);
        
    //     // Test case 17: String template with JSONPath interpolation
    //     let template17 = Value::String("{\"name\": \"{{inputs[0].name}}\", \"age\": {{inputs[0].age}}}".to_string());
    //     let variables17 = vec![Value::Object({
    //         let mut map = serde_json::Map::new();
    //         map.insert("name".to_string(), Value::String("Eve".to_string()));
    //         map.insert("age".to_string(), Value::Number(serde_json::Number::from(28)));
    //         map
    //     })];
    //     let _executed_steps17: HashMap<String, ShAction> = HashMap::new();
    //     let result17 = engine.interpolate_from_parent_inputs(&template17, &variables17).unwrap();
    //     let expected17 = Value::Object({
    //         let mut map = serde_json::Map::new();
    //         map.insert("name".to_string(), Value::String("Eve".to_string()));
    //         map.insert("age".to_string(), Value::Number(serde_json::Number::from(28)));
    //         map
    //     });
    //     assert_eq!(result17, expected17);
        
    //     // Test case 18: Empty object (should return unchanged)
    //     let template18 = Value::Object(serde_json::Map::new());
    //     let variables18 = vec![];
    //     let _executed_steps18: HashMap<String, ShAction> = HashMap::new();
    //     let result18 = engine.interpolate_from_parent_inputs(&template18, &variables18).unwrap();
    //     assert_eq!(result18, Value::Object(serde_json::Map::new()));
        
    //     // Test case 19: Empty array (should return unchanged)
    //     let template19 = Value::Array(vec![]);
    //     let variables19 = vec![];
    //     let _executed_steps19: HashMap<String, ShAction> = HashMap::new();
    //     let result19 = engine.interpolate_from_parent_inputs(&template19, &variables19).unwrap();
    //     assert_eq!(result19, Value::Array(vec![]));
        
    //     // Test case 20: String template with malformed JSON (should return as string)
    //     let template20 = Value::String("Hello {{inputs[0]}} world!".to_string());
    //     let variables20 = vec![Value::String("Frank".to_string())];
    //     let _executed_steps20: HashMap<String, ShAction> = HashMap::new();
    //     let result20 = engine.interpolate_from_parent_inputs(&template20, &variables20).unwrap();
    //     assert_eq!(result20, Value::String("Hello Frank world!".to_string()));
    // }

    // #[test]
    // fn test_resolve_io() {
    //     let engine = ExecutionEngine::new();
        
    //     // Test case 1: Simple IO resolution with string templates
    //     let io_definitions = vec![
    //         ShIO {
    //             name: "input1".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("{{inputs[0]}}".to_string()),
    //             value: None,
    //             required: true,
    //         },
    //         ShIO {
    //             name: "input2".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("{{inputs[1]}}".to_string()),
    //             value: None,
    //             required: true,
    //         }
    //     ];
    //     let io_values = vec![
    //         ShIO {
    //             name: "input1".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("test1".to_string()),
    //             value: Some(Value::String("Hello".to_string())),
    //             required: true,
    //         },
    //         ShIO {
    //             name: "input2".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("test2".to_string()),
    //             value: Some(Value::String("World".to_string())),
    //             required: true,
    //         }
    //     ];
    //     let _steps: HashMap<String, ShAction> = HashMap::new();
    //     let result = engine.resolve_from_parent_inputs(&io_definitions, &io_values).unwrap();
    //     let expected = vec![
    //         Value::String("Hello".to_string()),
    //         Value::String("World".to_string())
    //     ];
    //     assert_eq!(result, expected);
        
    //     // Test case 2: IO resolution with object templates
    //     let io_definitions2 = vec![
    //         ShIO {
    //             name: "config".to_string(),
    //             r#type: "object".to_string(),
    //             template: Value::Object({
    //                 let mut map = serde_json::Map::new();
    //                 map.insert("name".to_string(), Value::String("{{inputs[0]}}".to_string()));
    //                 map.insert("age".to_string(), Value::String("{{inputs[1]}}".to_string()));
    //                 map
    //             }),
    //             value: None,
    //             required: true,
    //         }
    //     ];
    //     let io_values2 = vec![
    //         ShIO {
    //             name: "name".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("Alice".to_string()),
    //             value: Some(Value::String("Alice".to_string())),
    //             required: true,
    //         },
    //         ShIO {
    //             name: "age".to_string(),
    //             r#type: "number".to_string(),
    //             template: Value::String("25".to_string()),
    //             value: Some(Value::Number(serde_json::Number::from(25))),
    //             required: true,
    //         }
    //     ];
    //     let result2 = engine.resolve_from_parent_inputs(&io_definitions2, &io_values2).unwrap();
    //     let expected2 = vec![Value::Object({
    //         let mut map = serde_json::Map::new();
    //         map.insert("name".to_string(), Value::String("Alice".to_string()));
    //         map.insert("age".to_string(), Value::Number(serde_json::Number::from(25)));
    //         map
    //     })];
    //     assert_eq!(result2, expected2);
        
    //     // Test case 3: IO resolution with step dependencies
    //     let io_definitions3 = vec![
    //         ShIO {
    //             name: "result".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("{{steps.step1.outputs[0]}}".to_string()),
    //             value: None,
    //             required: true,
    //         }
    //     ];
    //     let io_values3 = vec![];
    //     let _steps3 = {
    //         let mut map = HashMap::new();
    //         let step1 = ShAction {
    //             id: "step1".to_string(),
    //             name: "test_step".to_string(),
    //             kind: "wasm".to_string(),
    //             uses: "test/action:1.0.0".to_string(),
    //             inputs: vec![],
    //             outputs: vec![
    //                 ShIO {
    //                     name: "output1".to_string(),
    //                     r#type: "string".to_string(),
    //                     template: Value::String("test_output".to_string()),
    //                     value: Some(Value::String("Hello from step1".to_string())),
    //                     required: true,
    //                 }
    //             ],
    //             parent_action: None,
    //             steps: HashMap::new(),
    //             role: None,
    //             types: None,
    //             mirrors: vec![],
    //             permissions: None,
    //         };
    //         map.insert("step1".to_string(), step1);
    //         map
    //     };
    //     let result3 = engine.resolve_from_parent_inputs(&io_definitions3, &io_values3).unwrap();
    //     let expected3 = vec![Value::String("Hello from step1".to_string())];
    //     assert_eq!(result3, expected3);
        
    //     // Test case 4: IO resolution with mixed input and step dependencies
    //     let io_definitions4 = vec![
    //         ShIO {
    //             name: "message".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("{{inputs[0]}} used {{steps.step1.outputs[0]}}".to_string()),
    //             value: None,
    //             required: true,
    //         }
    //     ];
    //     let io_values4 = vec![
    //         ShIO {
    //             name: "user".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("John".to_string()),
    //             value: Some(Value::String("John".to_string())),
    //             required: true,
    //         }
    //     ];
    //     let result4 = engine.resolve_from_parent_inputs(&io_definitions4, &io_values4).unwrap();
    //     let expected4 = vec![Value::String("John used Hello from step1".to_string())];
    //     assert_eq!(result4, expected4);
        
    //     // Test case 5: IO resolution with JSONPath
    //     let io_definitions5 = vec![
    //         ShIO {
    //             name: "name".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("{{inputs[0].name}}".to_string()),
    //             value: None,
    //             required: true,
    //         }
    //     ];
    //     let io_values5 = vec![
    //         ShIO {
    //             name: "user".to_string(),
    //             r#type: "object".to_string(),
    //             template: Value::String("user_object".to_string()),
    //             value: Some(Value::Object({
    //                 let mut map = serde_json::Map::new();
    //                 map.insert("name".to_string(), Value::String("Bob".to_string()));
    //                 map.insert("age".to_string(), Value::Number(serde_json::Number::from(30)));
    //                 map
    //             })),
    //             required: true,
    //         }
    //     ];
    //     let result5 = engine.resolve_from_parent_inputs(&io_definitions5, &io_values5).unwrap();
    //     let expected5 = vec![Value::String("Bob".to_string())];
    //     assert_eq!(result5, expected5);
        
    //     // Test case 6: IO resolution with unresolved templates (should return None)
    //     let io_definitions6 = vec![
    //         ShIO {
    //             name: "result".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("{{steps.nonexistent.outputs[0]}}".to_string()),
    //             value: None,
    //             required: true,
    //         }
    //     ];
    //     let io_values6 = vec![];
    //     let result6 = engine.resolve_from_parent_inputs(&io_definitions6, &io_values6);
    //     assert_eq!(result6, None);
        
    //     // Test case 7: IO resolution with empty definitions (should return empty vector)
    //     let io_definitions7 = vec![];
    //     let io_values7 = vec![];
    //     let result7 = engine.resolve_from_parent_inputs(&io_definitions7, &io_values7).unwrap();
    //     assert_eq!(result7, vec![] as Vec<Value>);
        
    //     // Test case 8: IO resolution with null values
    //     let io_definitions8 = vec![
    //         ShIO {
    //             name: "input1".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("{{inputs[0]}}".to_string()),
    //             value: None,
    //             required: true,
    //         }
    //     ];
    //     let io_values8 = vec![
    //         ShIO {
    //             name: "input1".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("test".to_string()),
    //             value: None, // No value
    //             required: true,
    //         }
    //     ];
    //     let result8 = engine.resolve_from_parent_inputs(&io_definitions8, &io_values8).unwrap();
    //     let expected8 = vec![Value::Null];
    //     assert_eq!(result8, expected8);
        
    //     // Test case 9: IO resolution with array templates
    //     let io_definitions9 = vec![
    //         ShIO {
    //             name: "items".to_string(),
    //             r#type: "array".to_string(),
    //             template: Value::Array(vec![
    //                 Value::String("{{inputs[0]}}".to_string()),
    //                 Value::String("{{inputs[1]}}".to_string()),
    //                 Value::String("{{inputs[2]}}".to_string())
    //             ]),
    //             value: None,
    //             required: true,
    //         }
    //     ];
    //     let io_values9 = vec![
    //         ShIO {
    //             name: "item1".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("apple".to_string()),
    //             value: Some(Value::String("apple".to_string())),
    //             required: true,
    //         },
    //         ShIO {
    //             name: "item2".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("banana".to_string()),
    //             value: Some(Value::String("banana".to_string())),
    //             required: true,
    //         },
    //         ShIO {
    //             name: "item3".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("cherry".to_string()),
    //             value: Some(Value::String("cherry".to_string())),
    //             required: true,
    //         }
    //     ];
    //     let result9 = engine.resolve_from_parent_inputs(&io_definitions9, &io_values9).unwrap();
    //     let expected9 = vec![Value::Array(vec![
    //         Value::String("apple".to_string()),
    //         Value::String("banana".to_string()),
    //         Value::String("cherry".to_string())
    //     ])];
    //     assert_eq!(result9, expected9);
        
    //     // Test case 10: IO resolution with complex nested structure
    //     let io_definitions10 = vec![
    //         ShIO {
    //             name: "config".to_string(),
    //             r#type: "object".to_string(),
    //             template: Value::Object({
    //                 let mut map = serde_json::Map::new();
    //                 map.insert("user".to_string(), Value::String("{{inputs[0]}}".to_string()));
    //                 map.insert("data".to_string(), Value::Array(vec![
    //                     Value::String("{{steps.step1.outputs[0]}}".to_string()),
    //                     Value::String("{{steps.step2.outputs[0]}}".to_string())
    //                 ]));
    //                 map
    //             }),
    //             value: None,
    //             required: true,
    //         }
    //     ];
    //     let io_values10 = vec![
    //         ShIO {
    //             name: "user".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("Charlie".to_string()),
    //             value: Some(Value::String("Charlie".to_string())),
    //             required: true,
    //         }
    //     ];
    //     let _steps10 = {
    //         let mut map = HashMap::new();
    //         let step1 = ShAction {
    //             id: "step1".to_string(),
    //             name: "test_step1".to_string(),
    //             kind: "wasm".to_string(),
    //             uses: "test/action:1.0.0".to_string(),
    //             inputs: vec![],
    //             outputs: vec![
    //                 ShIO {
    //                     name: "result1".to_string(),
    //                     r#type: "string".to_string(),
    //                     template: Value::String("result1".to_string()),
    //                     value: Some(Value::String("Hello from step1".to_string())),
    //                     required: true,
    //                 }
    //             ],
    //             parent_action: None,
    //             steps: HashMap::new(),
    //             role: None,
    //             types: None,
    //             mirrors: vec![],
    //             permissions: None,
    //         };
    //         let step2 = ShAction {
    //             id: "step2".to_string(),
    //             name: "test_step2".to_string(),
    //             kind: "wasm".to_string(),
    //             uses: "test/action:1.0.0".to_string(),
    //             inputs: vec![],
    //             outputs: vec![
    //                 ShIO {
    //                     name: "result2".to_string(),
    //                     r#type: "string".to_string(),
    //                     template: Value::String("result2".to_string()),
    //                     value: Some(Value::String("Hello from step2".to_string())),
    //                     required: true,
    //                 }
    //             ],
    //             parent_action: None,
    //             steps: HashMap::new(),
    //             role: None,
    //             types: None,
    //             mirrors: vec![],
    //             permissions: None,
    //         };
    //         map.insert("step1".to_string(), step1);
    //         map.insert("step2".to_string(), step2);
    //         map
    //     };
    //     let result10 = engine.resolve_from_parent_inputs(&io_definitions10, &io_values10).unwrap();
    //     let expected10 = vec![Value::Object({
    //         let mut map = serde_json::Map::new();
    //         map.insert("user".to_string(), Value::String("Charlie".to_string()));
    //         map.insert("data".to_string(), Value::Array(vec![
    //             Value::String("Hello from step1".to_string()),
    //             Value::String("Hello from step2".to_string())
    //         ]));
    //         map
    //     })];
    //     assert_eq!(result10, expected10);
        
    //     // Test case 11: IO resolution with interpolation failure (should return None)
    //     let io_definitions11 = vec![
    //         ShIO {
    //             name: "result".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("{{inputs[0]}}".to_string()),
    //             value: None,
    //             required: true,
    //         }
    //     ];
    //     let io_values11 = vec![]; // No values provided
    //     let result11 = engine.resolve_from_parent_inputs(&io_definitions11, &io_values11);
    //     assert_eq!(result11, None);
        
    //     // Test case 12: IO resolution with still unresolved templates (should return None)
    //     let io_definitions12 = vec![
    //         ShIO {
    //             name: "result".to_string(),
    //             r#type: "string".to_string(),
    //             template: Value::String("{{steps.step1.outputs[0]}}".to_string()),
    //             value: None,
    //             required: true,
    //         }
    //     ];
    //     let io_values12 = vec![];
    //     let _steps12: HashMap<String, ShAction> = HashMap::new(); // No executed steps
    //     let result12 = engine.resolve_from_parent_inputs(&io_definitions12, &io_values12);
    //     assert_eq!(result12, None);
    // }

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
        
        let result1 = engine.cast_values_to_typed_array(&io_fields1, &input_values1, &types1);
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
        
        let result2 = engine.cast_values_to_typed_array(&io_fields2, &input_values2, &types1);
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
        
        let result3 = engine.cast_values_to_typed_array(&io_fields3, &input_values3, &types3);
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
        
        let result4 = engine.cast_values_to_typed_array(&io_fields4, &input_values4, &types3);
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
        
        let result5 = engine.cast_values_to_typed_array(&io_fields5, &input_values5, &types3);
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
        
        let result6 = engine.cast_values_to_typed_array(&io_fields6, &input_values6, &types6);
        assert!(result6.is_err());
        assert!(result6.unwrap_err().to_string().contains("Failed to compile schema for type 'InvalidType'"));
        
        // Test case 7: Empty IO fields and values
        let io_fields7 = vec![];
        let input_values7 = vec![];
        
        let result7 = engine.cast_values_to_typed_array(&io_fields7, &input_values7, &types1);
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
        
        let result8 = engine.cast_values_to_typed_array(&io_fields8, &input_values8, &types3);
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
        
        let result9 = engine.cast_values_to_typed_array(&io_fields9, &input_values9, &types9);
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
        
        let result10 = engine.cast_values_to_typed_array(&io_fields10, &input_values10, &types1);
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
        println!("outputs: {:#?}", outputs);
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

    #[tokio::test]
    async fn test_execute_action_create_do_project() {
        dotenv::dotenv().ok();

        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing action with the same inputs as test_build_action_tree
        let action_ref = "starthubhq/do-create-project:0.0.1";
        
        // Read test parameters from environment variables with defaults
        let api_token = std::env::var("DO_API_TOKEN")
            .unwrap_or_else(|_| "".to_string());
        let name = std::env::var("DO_PROJECT_NAME")
            .unwrap_or_else(|_| "".to_string());
        let description = std::env::var("DO_PROJECT_DESCRIPTION")
            .unwrap_or_else(|_| "".to_string());
        let purpose = std::env::var("DO_PROJECT_PURPOSE")
            .unwrap_or_else(|_| "".to_string());
        let environment = std::env::var("DO_PROJECT_ENVIRONMENT")
            .unwrap_or_else(|_| "".to_string());
        
        let inputs = vec![
            json!({
                "api_token": api_token,
                "name": name,
                "description": description,
                "purpose": purpose,
                "environment": environment
            })
        ];
        
        println!("inputs: {:#?}", inputs);
        let result = engine.execute_action(action_ref, inputs).await;
        
        println!("result: {:#?}", result);
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid action_ref and inputs");
    }

    #[tokio::test]
    async fn test_execute_action_create_do_tag() {
        dotenv::dotenv().ok();

        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing action for tag creation
        let action_ref = "starthubhq/do-create-tag:0.0.1";
        
        // Read test parameters from environment variables with defaults
        let api_token = std::env::var("DO_API_TOKEN")
            .unwrap_or_else(|_| "".to_string());
        let name = std::env::var("DO_TAG_NAME")
            .unwrap_or_else(|_| "test-tag".to_string());
        
        let inputs = vec![
            json!({
                "api_token": api_token,
                "name": name
            })
        ];
        
        println!("inputs: {:#?}", inputs);
        let result = engine.execute_action(action_ref, inputs).await;
        
        println!("result: {:#?}", result);
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid action_ref and inputs");
    }

    #[tokio::test]
    async fn test_execute_action_create_do_vpc() {
        dotenv::dotenv().ok();

        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing action for VPC creation
        let action_ref = "starthubhq/do-create-vpc:0.0.1";
        
        // Read test parameters from environment variables with defaults
        let api_token = std::env::var("DO_API_TOKEN")
            .unwrap_or_else(|_| "".to_string());
        let name = std::env::var("DO_VPC_NAME")
            .unwrap_or_else(|_| "test-vpc".to_string());
        let region = std::env::var("DO_VPC_REGION")
            .unwrap_or_else(|_| "nyc1".to_string());
        let ip_range = std::env::var("DO_VPC_IP_RANGE")
            .unwrap_or_else(|_| "10.10.10.0/24".to_string());
        let description = std::env::var("DO_VPC_DESCRIPTION")
            .unwrap_or_else(|_| "Test VPC for development".to_string());
        
        let inputs = vec![
            json!({
                "api_token": api_token,
                "name": name,
                "region": region,
                "ip_range": ip_range,
                "description": description,
                "default": false
            })
        ];
        
        let result = engine.execute_action(action_ref, inputs).await;
        
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid action_ref and inputs");
    }

    #[tokio::test]
    async fn test_execute_action_create_do_db_sync() {
        dotenv::dotenv().ok();

        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing action for database creation
        let action_ref = "starthubhq/do-create-db-sync:0.0.1";
        
        // Read test parameters from environment variables with defaults
        let api_token = std::env::var("DO_API_TOKEN")
            .unwrap_or_else(|_| "".to_string());
        let name = std::env::var("DO_DB_NAME")
            .unwrap_or_else(|_| "test-database".to_string());
        let engine_type = std::env::var("DO_DB_ENGINE")
            .unwrap_or_else(|_| "pg".to_string());
        let region = std::env::var("DO_DB_REGION")
            .unwrap_or_else(|_| "nyc1".to_string());
        let size = std::env::var("DO_DB_SIZE")
            .unwrap_or_else(|_| "db-s-1vcpu-1gb".to_string());
        let num_nodes = std::env::var("DO_DB_NUM_NODES")
            .unwrap_or_else(|_| "1".to_string())
            .parse::<i32>()
            .unwrap_or(1);
        
        let inputs = vec![
            json!({
                "api_token": api_token,
                "name": name,
                "engine": engine_type,
                "region": region,
                "size": size,
                "num_nodes": num_nodes
            })
        ];
        
        println!("inputs: {:#?}", inputs);
        let result = engine.execute_action(action_ref, inputs).await;
        
        println!("result: {:#?}", result);
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid action_ref and inputs");
    }

    #[tokio::test]
    async fn test_execute_action_get_do_db() {
        dotenv::dotenv().ok();

        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing action for database status retrieval
        let action_ref = "starthubhq/do-get-db:0.0.1";
        
        // Read test parameters from environment variables with defaults
        let api_token = std::env::var("DO_API_TOKEN")
            .unwrap_or_else(|_| "".to_string());
        let database_id = std::env::var("DO_DB_ID")
            .unwrap_or_else(|_| "test-database-id".to_string());
        
        let inputs = vec![
            json!({
                "api_token": api_token,
                "database_id": database_id
            })
        ];
        
        println!("inputs: {:#?}", inputs);
        let result = engine.execute_action(action_ref, inputs).await;
        
        println!("result: {:#?}", result);
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid action_ref and inputs");
    }

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

    #[tokio::test]
    async fn test_execute_action_create_do_droplet_sync() {
        dotenv::dotenv().ok();

        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing action for droplet creation with sync
        let action_ref = "starthubhq/do-create-droplet-sync:0.0.1";
        
        // Read test parameters from environment variables with defaults
        let api_token = std::env::var("DO_API_TOKEN")
            .unwrap_or_else(|_| "".to_string());
        let name = std::env::var("DO_DROPLET_NAME")
            .unwrap_or_else(|_| "test-droplet-sync".to_string());
        let region = std::env::var("DO_DROPLET_REGION")
            .unwrap_or_else(|_| "nyc1".to_string());
        let size = std::env::var("DO_DROPLET_SIZE")
            .unwrap_or_else(|_| "s-1vcpu-1gb".to_string());
        let image = std::env::var("DO_DROPLET_IMAGE")
            .unwrap_or_else(|_| "ubuntu-20-04-x64".to_string());
        let backups = std::env::var("DO_DROPLET_BACKUPS")
            .unwrap_or_else(|_| "false".to_string());
        let ipv6 = std::env::var("DO_DROPLET_IPV6")
            .unwrap_or_else(|_| "false".to_string());
        let monitoring = std::env::var("DO_DROPLET_MONITORING")
            .unwrap_or_else(|_| "false".to_string());
        let tags = std::env::var("DO_DROPLET_TAGS")
            .unwrap_or_else(|_| "".to_string());
        let user_data = std::env::var("DO_DROPLET_USER_DATA")
            .unwrap_or_else(|_| "".to_string());
        
        // Parse boolean values
        let backups_bool = backups.parse::<bool>().unwrap_or(false);
        let ipv6_bool = ipv6.parse::<bool>().unwrap_or(false);
        let monitoring_bool = monitoring.parse::<bool>().unwrap_or(false);
        
        // Parse array values
        let tags_array: Vec<String> = if tags.is_empty() {
            vec![]
        } else {
            tags.split(',').map(|s| s.trim().to_string()).collect()
        };
        
        let inputs = vec![
            json!({
                "api_token": api_token,
                "name": name,
                "region": region,
                "size": size,
                "image": image,
                "backups": backups_bool,
                "ipv6": ipv6_bool,
                "monitoring": monitoring_bool,
                "tags": tags_array,
                "user_data": user_data
            })
        ];
        
        // println!("inputs: {:#?}", inputs);
        let result = engine.execute_action(action_ref, inputs).await;
        println!("result: {:#?}", result);
        // The test should succeed
        // assert!(result.is_ok(), "execute_action should succeed for valid action_ref and inputs");
    }

    #[tokio::test]
    async fn test_execute_action_get_do_droplet() {
        dotenv::dotenv().ok();

        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing action for droplet retrieval
        let action_ref = "starthubhq/do-get-droplet:0.0.1";
        
        // Read test parameters from environment variables with defaults
        let api_token = std::env::var("DO_API_TOKEN")
            .unwrap_or_else(|_| "".to_string());
        let droplet_id = std::env::var("DO_DROPLET_ID")
            .unwrap_or_else(|_| "123456789".to_string());
        
        println!("DO_API_TOKEN from env: '{}'", std::env::var("DO_API_TOKEN").unwrap_or_default());
        println!("DO_DROPLET_ID from env: '{}'", std::env::var("DO_DROPLET_ID").unwrap_or_default());
        
        let inputs = vec![
            json!({
                "api_token": api_token,
                "droplet_id": droplet_id.parse::<u64>().unwrap_or(0)
            })
        ];
        
        println!("inputs: {:#?}", inputs);
        let result = engine.execute_action(action_ref, inputs).await;
        
        println!("do-get-droplet test result: {:#?}", result);
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid do-get-droplet action_ref and inputs");        
    }

    #[tokio::test]
    async fn test_execute_action_ssh() {
        dotenv::dotenv().ok();
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();

        // Docker-based SSH action
        let action_ref = "starthubhq/ssh:0.0.1";

        // Read test parameters from environment variables, with fallback to default SSH key
        let private_key = std::env::var("SSH_PRIVATE_KEY")
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                std::fs::read_to_string(format!("{}/.ssh/id_rsa", home))
                    .unwrap_or_else(|_| "".to_string())
            });
        let user = std::env::var("SSH_USER").unwrap_or_default();
        let host = std::env::var("SSH_HOST").unwrap_or_default();
        let cmd = std::env::var("SSH_COMMAND").unwrap_or_default();

        println!("private_key: {}", private_key);
        println!("user: {}", user);
        println!("host: {}", host);
        println!("cmd: {}", cmd);
        // If required env vars are missing, skip the test gracefully
        if private_key.is_empty() || user.is_empty() || host.is_empty() {
            println!("Skipping SSH test: missing SSH_PRIVATE_KEY/SSH_USER/SSH_HOST env vars");
            return;
        }

        let inputs = vec![
            json!(private_key),
            json!(user),
            json!(host),
            json!(cmd),
        ];

        let result = engine.execute_action(action_ref, inputs).await;
        println!("ssh test result: {:#?}", result);

        // Don't hard fail CI if remote isn't reachable; this validates the execution path
        if let Ok(outputs) = result {
            assert!(outputs.is_array());
            let outputs_array = outputs.as_array().unwrap();
            assert!(!outputs_array.is_empty());
        }
    }

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
        
        let result = engine.execute_action(action_ref, inputs).await;
        
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid http-get-wasm action_ref and inputs");
        
        // // Verify that we got outputs
        // assert!(outputs.is_array(), "execute_action should return an array of outputs");
        let outputs = result.unwrap();        
        
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
            json!(1),  // simulator_id as string
            json!("sb_publishable_AKGy20M54_uMOdJme3ZnZA_GX11LgHe")  // api_key as string
        ];
        
        let result = engine.execute_action(action_ref, inputs).await;
        
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
            json!(1),  // simulator_id as string
            json!("sb_publishable_AKGy20M54_uMOdJme3ZnZA_GX11LgHe")  // api_key as string
        ];
        
        let result = engine.execute_action(action_ref, inputs).await;
        
        // print the outputs
        let outputs = result.unwrap();
        println!("outputs: {:#?}", outputs);
    }
    
    #[tokio::test]
    async fn test_execute_action_openweather_coordinates_by_location_name() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing the openweather-coordinates-by-location-name action
        let action_ref = "starthubhq/openweather-coordinates-by-location-name:0.0.1";
        
        // Test with location name and API key
        let inputs = vec![
            json!({
                "location_name": "London",
                "open_weather_api_key": "f13e712db9557544db878888528a5e29"
            })
        ];
        
        let result = engine.execute_action(action_ref, inputs).await;
        
        // The test should succeed in terms of action parsing and execution setup
        // Note: The actual API call might fail due to invalid API key, but the composition should be properly executed
        assert!(result.is_ok(), "execute_action should succeed for valid openweather-coordinates-by-location-name action_ref and inputs");
        
        let outputs = result.unwrap();
                // Verify that we got outputs
        assert!(outputs.is_array(), "execute_action should return an array of outputs");
        let outputs_array = outputs.as_array().unwrap();
        
        // For a composition action, we expect at least one output
        assert!(!outputs_array.is_empty(), "Composition action should produce outputs");
        
        // Verify the output structure - should contain geocoding response
        let first_output = &outputs_array[0];
        assert!(first_output.is_object(), "Output should be an object");
        
        let output_obj = first_output.as_object().unwrap();
        
        // Verify the output has the expected structure based on the starthub-lock.json
        // The output should contain coordinates, name, country, state, and local_names
        assert!(output_obj.contains_key("name"), "Output should contain 'name' field");
        assert!(output_obj.contains_key("lat"), "Output should contain 'lat' field");
        assert!(output_obj.contains_key("lon"), "Output should contain 'lon' field");
        assert!(output_obj.contains_key("country"), "Output should contain 'country' field");
        assert!(output_obj.contains_key("state"), "Output should contain 'state' field");
        assert!(output_obj.contains_key("local_names"), "Output should contain 'local_names' field");
        
        // Verify local_names is an object with expected language keys
        if let Some(local_names) = output_obj.get("local_names") {
            assert!(local_names.is_object(), "local_names should be an object");
            let local_names_obj = local_names.as_object().unwrap();
            
            // Check for some expected language keys (based on the starthub-lock.json)
            let expected_languages = ["en", "it", "fr", "de", "es", "pt", "ru", "zh", "ja", "ko", "ar", "hi"];
            for lang in expected_languages {
                if local_names_obj.contains_key(lang) {
                    assert!(local_names_obj[lang].is_string(), "local_names.{} should be a string", lang);
                }
            }
        }
        
        // Verify coordinate types
        if let Some(lat) = output_obj.get("lat") {
            assert!(lat.is_number(), "lat should be a number");
        }
        if let Some(lon) = output_obj.get("lon") {
            assert!(lon.is_number(), "lon should be a number");
        }
        
        // Verify string fields
        if let Some(name) = output_obj.get("name") {
            assert!(name.is_string(), "name should be a string");
        }
        if let Some(country) = output_obj.get("country") {
            assert!(country.is_string(), "country should be a string");
        }
        if let Some(state) = output_obj.get("state") {
            assert!(state.is_string(), "state should be a string");
        }
    }

    #[tokio::test]
    async fn test_execute_action_openweather_current_weather() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing the openweather-current-weather action
        let action_ref = "starthubhq/openweather-current-weather:0.0.1";
        
        // Test with latitude, longitude, and API key
        let inputs = vec![
            json!({
                "lat": 51.5074,
                "lon": -0.1278,
                "open_weather_api_key": "f13e712db9557544db878888528a5e29"
            })
        ];
        
        let result = engine.execute_action(action_ref, inputs).await;
        
        // The test should succeed in terms of action parsing and execution setup
        // Note: The actual API call might fail due to invalid API key, but the composition should be properly executed
        match result {
            Ok(outputs) => {
                // Verify that we got outputs
                assert!(outputs.is_array(), "execute_action should return an array of outputs");
                let outputs_array = outputs.as_array().unwrap();
                
                // For a composition action, we expect at least one output
                assert!(!outputs_array.is_empty(), "Composition action should produce outputs");
                
                // Verify the output structure - should contain weather response
                let first_output = &outputs_array[0];
                assert!(first_output.is_object(), "Output should be an object");
                
                let output_obj = first_output.as_object().unwrap();
                
                // Verify the output has the expected structure based on the starthub-lock.json
                // The output should contain weather data with coord, weather, main, wind, etc.
                assert!(output_obj.contains_key("coord"), "Output should contain 'coord' field");
                assert!(output_obj.contains_key("weather"), "Output should contain 'weather' field");
                assert!(output_obj.contains_key("main"), "Output should contain 'main' field");
                assert!(output_obj.contains_key("wind"), "Output should contain 'wind' field");
                assert!(output_obj.contains_key("clouds"), "Output should contain 'clouds' field");
                assert!(output_obj.contains_key("sys"), "Output should contain 'sys' field");
                assert!(output_obj.contains_key("name"), "Output should contain 'name' field");
                assert!(output_obj.contains_key("cod"), "Output should contain 'cod' field");
                
                // Verify coord structure
                if let Some(coord) = output_obj.get("coord") {
                    assert!(coord.is_object(), "coord should be an object");
                    let coord_obj = coord.as_object().unwrap();
                    assert!(coord_obj.contains_key("lon"), "coord should contain 'lon' field");
                    assert!(coord_obj.contains_key("lat"), "coord should contain 'lat' field");
                }
                
                // Verify weather array structure
                if let Some(weather) = output_obj.get("weather") {
                    assert!(weather.is_array(), "weather should be an array");
                    let weather_array = weather.as_array().unwrap();
                    if !weather_array.is_empty() {
                        let first_weather = &weather_array[0];
                        assert!(first_weather.is_object(), "weather item should be an object");
                        let weather_obj = first_weather.as_object().unwrap();
                        assert!(weather_obj.contains_key("id"), "weather item should contain 'id' field");
                        assert!(weather_obj.contains_key("main"), "weather item should contain 'main' field");
                        assert!(weather_obj.contains_key("description"), "weather item should contain 'description' field");
                        assert!(weather_obj.contains_key("icon"), "weather item should contain 'icon' field");
                    }
                }
                
                // Verify main structure
                if let Some(main) = output_obj.get("main") {
                    assert!(main.is_object(), "main should be an object");
                    let main_obj = main.as_object().unwrap();
                    assert!(main_obj.contains_key("temp"), "main should contain 'temp' field");
                    assert!(main_obj.contains_key("feels_like"), "main should contain 'feels_like' field");
                    assert!(main_obj.contains_key("pressure"), "main should contain 'pressure' field");
                    assert!(main_obj.contains_key("humidity"), "main should contain 'humidity' field");
                }
                
                // Verify wind structure
                if let Some(wind) = output_obj.get("wind") {
                    assert!(wind.is_object(), "wind should be an object");
                    let wind_obj = wind.as_object().unwrap();
                    assert!(wind_obj.contains_key("speed"), "wind should contain 'speed' field");
                    assert!(wind_obj.contains_key("deg"), "wind should contain 'deg' field");
                }
                
                // Verify clouds structure
                if let Some(clouds) = output_obj.get("clouds") {
                    assert!(clouds.is_object(), "clouds should be an object");
                    let clouds_obj = clouds.as_object().unwrap();
                    assert!(clouds_obj.contains_key("all"), "clouds should contain 'all' field");
                }
                
                // Verify sys structure
                if let Some(sys) = output_obj.get("sys") {
                    assert!(sys.is_object(), "sys should be an object");
                    let sys_obj = sys.as_object().unwrap();
                    assert!(sys_obj.contains_key("country"), "sys should contain 'country' field");
                    assert!(sys_obj.contains_key("sunrise"), "sys should contain 'sunrise' field");
                    assert!(sys_obj.contains_key("sunset"), "sys should contain 'sunset' field");
                }
            }
            Err(e) => {
                // If the execution fails (e.g., due to API key issues), that's expected for this test
                // We just want to ensure the composition is properly parsed and the structure is correct
                println!("Expected execution failure due to test API key: {}", e);
                
                // The important thing is that the composition was parsed correctly
                // and the execution engine attempted to run it
                assert!(e.to_string().contains("No output value found") || 
                       e.to_string().contains("HTTP") || 
                       e.to_string().contains("API") ||
                       e.to_string().contains("network") ||
                       e.to_string().contains("Unresolved templates"),
                       "Expected API-related error, got: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_execute_action_number_to_string() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing the number-to-string action
        let action_ref = "std/number-to-string:0.0.1";
        
        // Test with a number input
        let inputs = vec![
            json!(42.5)
        ];
        
        let result = engine.execute_action(action_ref, inputs).await;
        
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid number-to-string action_ref and inputs");
        
        let outputs = result.unwrap();
        // Verify that we got outputs
        assert!(outputs.is_array(), "execute_action should return an array of outputs");
        let outputs_array = outputs.as_array().unwrap();
        
        // For a WASM action, we expect at least one output
        assert!(!outputs_array.is_empty(), "WASM action should produce outputs");
        
        // Verify the output structure - should contain the converted string
        let first_output = &outputs_array[0];
        assert!(first_output.is_string(), "Output should be a string");
        
        let output_string = first_output.as_str().unwrap();
        assert_eq!(output_string, "42.5", "Output should be the string representation of the input number");
    }

    #[tokio::test]
    async fn test_execute_action_number_to_string_integer() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing the number-to-string action with an integer
        let action_ref = "std/number-to-string:0.0.1";
        
        // Test with an integer input
        let inputs = vec![
            json!(100)
        ];
        
        let result = engine.execute_action(action_ref, inputs).await;
        
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid number-to-string action_ref and integer inputs");
        
        let outputs = result.unwrap();
        
        // Verify that we got outputs
        assert!(outputs.is_array(), "execute_action should return an array of outputs");
        let outputs_array = outputs.as_array().unwrap();
        
        // For a WASM action, we expect at least one output
        assert!(!outputs_array.is_empty(), "WASM action should produce outputs");
        
        // Verify the output structure - should contain the converted string
        let first_output = &outputs_array[0];
        assert!(first_output.is_string(), "Output should be a string");
        
        let output_string = first_output.as_str().unwrap();
        assert_eq!(output_string, "100", "Output should be the string representation of the input integer");
    }

    #[tokio::test]
    async fn test_execute_action_number_to_string_negative() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing the number-to-string action with a negative number
        let action_ref = "std/number-to-string:0.0.1";
        
        // Test with a negative number input
        let inputs = vec![
            json!(-15.7)
        ];
        
        let result = engine.execute_action(action_ref, inputs).await;
        
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid number-to-string action_ref and negative inputs");
        
        let outputs = result.unwrap();
        
        // Verify that we got outputs
        assert!(outputs.is_array(), "execute_action should return an array of outputs");
        let outputs_array = outputs.as_array().unwrap();
        
        // For a WASM action, we expect at least one output
        assert!(!outputs_array.is_empty(), "WASM action should produce outputs");
        
        // Verify the output structure - should contain the converted string
        let first_output = &outputs_array[0];
        assert!(first_output.is_string(), "Output should be a string");
        
        let output_string = first_output.as_str().unwrap();
        assert_eq!(output_string, "-15.7", "Output should be the string representation of the input negative number");
    }
    

    
}