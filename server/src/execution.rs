use anyhow::Result;
use jsonschema::JSONSchema;
use serde_json::Value;
use std::collections::HashMap;
use dirs;
use tokio::sync::broadcast;

use crate::models::{ShManifest, ShKind, ShIO, ShAction, ShExecutionFrame, ShType};
use crate::wasm;
use crate::logger::{Logger, Loggable};

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

    // Main flow
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
        let mut root_action = self.build_action_tree(
            action_ref,         // Action reference to download
            None,               // No parent action ID (root)
        ).await?;     

        // return Ok(serde_json::to_value(root_action)?);
        self.logger.log_success("Action tree built successfully", Some(&root_action.id));

        self.logger.log_info("Executing action tree...", Some(&root_action.id));
        let result = self.run_action_tree(&mut root_action,
            &inputs, &HashMap::new()).await?;
        self.logger.log_success("Action execution completed", Some(&root_action.id));


        // Return the action tree (no execution)
        Ok(serde_json::to_value(result)?)
    }

    async fn run_action_tree(&mut self,
        action: &mut ShAction,
        inputs: &Vec<Value>,
        siblings: &HashMap<String, ShAction>) -> Result<ShAction> {
        
        // 1) Instantiate and assign the inputs according to the types specified
        self.assign_io(&mut action.inputs, &inputs, &action.types)?;

        // Base condition.
        if action.kind == "wasm" || action.kind == "docker" {
            println!("Executing {} wasm step: {}", action.kind, action.name);
            self.logger.log_info(&format!("Executing {} wasm step: {}", action.kind, action.name), Some(&action.id));
            
            // Serialize the instantiated inputs
            let inputs_value = serde_json::to_value(&action.inputs)?;

            let mut result = Vec::new();
            if   action.kind == "wasm" {
                result = wasm::run_wasm_step(
                    action, 
                    &inputs_value, 
                    &self.cache_dir,
                    &|msg, id| self.logger.log_info(msg, id),
                    &|msg, id| self.logger.log_success(msg, id),
                    &|msg, id| self.logger.log_error(msg, id),
                ).await?;
            } else if action.kind == "docker" {
                // TODO: Implement docker step execution
                return Err(anyhow::anyhow!("Docker step execution not implemented"));
            } else {
                return Err(anyhow::anyhow!("Unsupported action kind: {}", action.kind));
            }
            
            self.logger.log_success(&format!("{} step completed: {}", action.kind, action.name), Some(&action.id));

            // Parse the result into a vector of JSON objects
            let json_objects: Vec<Value> = if result.is_empty() {
                Vec::new()
            } else {
                if let Some(first_result) = result.first() {
                    if let Some(array) = first_result.as_array() {
                        array.iter().map(|item| Self::parse(item.clone())).collect()
                    } else {
                        vec![Self::parse(first_result.clone())]
                    }
                } else {
                    Vec::new()
                }
            };

            let mut action_state_clone = action.clone();
            
            // Instantiate and assign the outputs
            self.assign_io(&mut action_state_clone.outputs, &json_objects, &action.types)?;

            return Ok(action_state_clone);
        }

        // Simple output-driven execution loop using the action tree directly
        let mut iteration_count = 0;
        let max_iterations = 1000; // Prevent infinite loops
        
        loop {
            iteration_count += 1;
            if iteration_count > max_iterations {
                return Err(anyhow::anyhow!("Maximum iterations exceeded"));
            }
            
            println!("Checking if all parent outputs can be resolved");
            // 1. Check if all parent outputs can be resolved
            // if self.can_resolve_all_final_outputs(action)? {
            //     println!("All outputs can be resolved");
            //     break; // We're done!
            // }

            println!("Finding next executable step");
            
            // 2. Find next executable step
            let next_step_id = match self.find_next_executable_step(&action.steps)? {
                Some(step_id) => step_id,
                None => {
                    // No more steps can be executed
                    return Err(anyhow::anyhow!("Deadlock: no steps can be executed"));
                }
            };

            println!("next_step_id: {}", next_step_id);
            
            // Clone action state to avoid borrowing issues
            let action_state_clone = action.clone();
            
            if let Some(step) = action.steps.get_mut(&next_step_id) {
                self.logger.log_info(&format!("Starting step: {}", next_step_id), Some(&action.id));

                // Resolve inputs for this step using the action tree (common for both paths)
                let resolved_inputs_to_inject_into_child_step = self.resolve_io(
                    &step.inputs,
                    &action_state_clone.inputs,
                    &action_state_clone.steps
                )?;

                println!("resolved_inputs_to_inject_into_child_step: {:#?}", resolved_inputs_to_inject_into_child_step);
                // Regular step execution
                let processed_child = Box::pin(self.run_action_tree(
                    step, 
                    &resolved_inputs_to_inject_into_child_step,
                    &HashMap::new()
                )).await?;
                
                // Update the action state with the processed child
                action.steps.insert(next_step_id.clone(), processed_child);
                
                self.logger.log_success(&format!("Completed step: {}", next_step_id), Some(&action.id));
                
            } else {
                // Step not found, this shouldn't happen with dynamic execution
                self.logger.log_error(&format!("Step '{}' not found in action tree", next_step_id), Some(&action.id));
                break;
            }
        }

        println!("just before resolve_into_outputs");
        // If we got here, it means that we have executed all the steps in the action tree.
        // Now we need to aggregate the outputs back at the higher level.
        let resolved_outputs = self.resolve_io(
            &action.outputs,
            &action.inputs,
            &action.steps
        )?;

        // Assign resolved outputs to their corresponding fields
        self.assign_io(&mut action.outputs, &resolved_outputs, &action.types)?;

        return Ok(action.clone());
    }

    /// Parses a value to a JSON object or array
    fn parse(value: Value) -> Value {
        match value {
            Value::Object(mut obj) => {
                for (_, val) in obj.iter_mut() {
                    *val = Self::parse(val.clone());
                }
                Value::Object(obj)
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
    fn assign_io(
        &self,
        io_fields: &mut Vec<ShIO>,
        input_values: &Vec<Value>,
        types: &Option<serde_json::Map<String, Value>>
    ) -> Result<()> {
        let cast_values = self.cast(
            types,
            io_fields,
            input_values
        )?;

        for (index, io_field) in io_fields.iter_mut().enumerate() {
            if let Some(resolved_value) = cast_values.get(index) {
                io_field.value = Some(resolved_value.clone());
            }
        }

        Ok(())
    }

    /// Casts values to the appropriate type
    fn cast(&self, types: &Option<serde_json::Map<String, Value>>, 
        io_definitions: &Vec<ShIO>,
        io_values: &Vec<Value>) -> Result<Vec<Value>> {
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
                }
            }
        }
        Ok(values_to_inject)
    }

    /// Resolves IO definitions using input values and steps context
    fn resolve_io(
        &self,
        io_definitions: &Vec<ShIO>,
        io_values: &Vec<ShIO>,
        steps: &HashMap<String, ShAction>
    ) -> Result<Vec<Value>> {
        let mut resolved_values: Vec<Value> = Vec::new();

        // Extract values from the input values vector
        let values: Vec<Value> = io_values.iter()
            .map(|io| io.value.clone().unwrap_or(Value::Null))
            .collect();

        // For every definition, resolve its template
        for (index, _definition) in io_definitions.iter().enumerate() {
            if let Some(definition) = io_definitions.get(index) {
                // Resolve the template to get the actual value
                let interpolated_template = self.interpolate(
                    &definition.template, 
                    &values,
                    steps
                )?;

                resolved_values.push(interpolated_template);
            }
        }

        Ok(resolved_values)
    }

    // Since the variables might becoming from the parent or the siblings, this
    // function needs to know the parent inputs and the steps that have already been executed.
    fn interpolate(&self, 
        template: &Value, 
        variables: &Vec<Value>, 
        executed_steps: &HashMap<String, ShAction>
    ) -> Result<Value> {
        match template {
            Value::String(s) => {
                // println!("resolve_template_string: {:#?}", s);
                let resolved = self.interpolate_string(s, variables, executed_steps)?;
                
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
                    let resolved_value = self.interpolate(value, variables, executed_steps)?;
                    resolved_obj.insert(key.clone(), resolved_value);
                }
                
                Ok(Value::Object(resolved_obj))
            },
            Value::Array(arr) => {
                // println!("resolve_template_array: {:#?}", arr);
                // Recursively resolve array templates
                let mut resolved_arr = Vec::new();
                for item in arr {
                    let resolved_item = self.interpolate(item, variables, executed_steps)?;
                    resolved_arr.push(resolved_item);
                }
                Ok(Value::Array(resolved_arr))
            },
            _ => Ok(template.clone())
        }
    }

    fn interpolate_string(&self, 
        template: &str, 
        variables: &Vec<Value>, 
        executed_steps: &HashMap<String, ShAction>
    ) -> Result<String> {
        let mut result = template.to_string();
        
        // Handle {{inputs[index]}} patterns (without jsonpath)
        let inputs_simple_re = regex::Regex::new(r"\{\{inputs\[(\d+)\]\}\}")?;
        for cap in inputs_simple_re.captures_iter(template) {
            if let Some(index_str) = cap.get(1) {
                if let Ok(index) = index_str.as_str().parse::<usize>() {
                    if let Some(input_value) = variables.get(index) {
                        let replacement = match input_value {
                            Value::String(s) => s.clone(),
                            _ => input_value.to_string(),
                        };
                        result = result.replace(&cap[0], &replacement);
                    }
                }
            }
        }
        
        // Handle {{inputs[index].jsonpath}} patterns
        let inputs_re = regex::Regex::new(r"\{\{inputs\[(\d+)\]\.([^}]+)\}\}")?;
        for cap in inputs_re.captures_iter(template) {
            if let (Some(index_str), Some(jsonpath)) = (cap.get(1), cap.get(2)) {
                if let Ok(index) = index_str.as_str().parse::<usize>() {
                    if let Some(input_value) = variables.get(index) {
                        if let Ok(resolved_value) = self.evaluate_jsonpath(input_value, jsonpath.as_str()) {
                            let replacement = match resolved_value {
                                Value::String(s) => s.clone(),
                                _ => resolved_value.to_string(),
                            };
                            result = result.replace(&cap[0], &replacement);
                        }
                    }
                }
            }
        }
        
        // Handle {{steps.step_name.outputs[index]}} patterns (without jsonpath)
        let steps_simple_re = regex::Regex::new(r"\{\{steps\.([^.]+)\.outputs\[(\d+)\]\}\}")?;
        for cap in steps_simple_re.captures_iter(template) {
            if let (Some(step_name), Some(index_str)) = (cap.get(1), cap.get(2)) {
                if let Ok(index) = index_str.as_str().parse::<usize>() {
                    if let Some(step) = executed_steps.get(step_name.as_str()) {
                        if let Some(output) = step.outputs.get(index) {
                            if let Some(output_value) = &output.value {
                                let replacement = match output_value {
                                    Value::String(s) => s.clone(),
                                    _ => output_value.to_string(),
                                };
                                result = result.replace(&cap[0], &replacement);
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
        }
        
        // Handle {{steps.step_name.outputs[index].jsonpath}} patterns
        let steps_re = regex::Regex::new(r"\{\{steps\.([^.]+)\.outputs\[(\d+)\]\.([^}]+)\}\}")?;
        for cap in steps_re.captures_iter(template) {
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
                                    
                                    result = result.replace(&cap[0], &replacement);
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
        }
        
        Ok(result)
    }

    fn evaluate_jsonpath(&self, value: &Value, jsonpath: &str) -> Result<Value> {
        // Simple JSONPath evaluation for common patterns
        let path_parts: Vec<&str> = jsonpath.split('.').collect();
        let mut current = value;
        for part in path_parts {
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
                    
                    let mut properties = serde_json::Map::new();
                    let mut required = Vec::new();
                    
                    for (field_name, field_def) in obj {
                        if let Ok(converted_field) = self.convert_to_json_schema(field_def) {
                            properties.insert(field_name.clone(), converted_field);
                            
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
            flow_control: manifest.flow_control,
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

        // At this point we have resolved all the children
        // Steps will be executed dynamically based on dependencies during runtime

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

    /// Checks if all parent outputs can be resolved using the action tree
    fn can_resolve_all_final_outputs(
        &self,
        action: &ShAction,
    ) -> Result<bool> {
        // Use resolve_io to get the resolved outputs
        let resolved_outputs = match self.resolve_io(
            &action.outputs,
            &action.inputs,
            &action.steps
        ) {
            Ok(outputs) => outputs,
            Err(_) => return Ok(false), // Cannot resolve outputs yet
        };
        
        // Check if any resolved output still contains template syntax ({{ or }})
        for output in resolved_outputs {
            if self.contains_unresolved_templates(&output) {
                return Ok(false); // Found unresolved templates
            }
        }
        
        Ok(true) // All outputs are fully resolved
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

    /// Finds the next executable step using the action tree
    fn find_next_executable_step(
        &self,
        steps: &HashMap<String, ShAction>,
    ) -> Result<Option<String>> {
        // TODO: Implement step selection logic
        // This should find the next step that can be executed based on:
        // 1. Dependencies are satisfied
        // 2. Inputs can be resolved
        // 3. Step is not already executed or skipped
        Ok(None)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    
    use serde_json::json;

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
        
        let action_tree = result.unwrap();
        
        // Verify the root action structure
        assert_eq!(action_tree["name"], "get-weather-by-location-name");
        assert_eq!(action_tree["kind"], "composition");
        assert_eq!(action_tree["uses"], action_ref);
        assert!(action_tree["parent_action"].is_null());
        
        // Verify inputs
        assert!(action_tree["inputs"].is_array());
        let inputs_array = action_tree["inputs"].as_array().unwrap();
        assert_eq!(inputs_array.len(), 1);
        let input = &inputs_array[0];
        assert_eq!(input["name"], "weather_config");
        assert_eq!(input["type"], "WeatherConfig");
        
        // Verify outputs
        assert!(action_tree["outputs"].is_array());
        let outputs_array = action_tree["outputs"].as_array().unwrap();
        assert_eq!(outputs_array.len(), 1);
        let output = &outputs_array[0];
        assert_eq!(output["name"], "response");
        assert_eq!(output["type"], "CustomWeatherResponse");
        
        // Execution order is now determined dynamically at runtime
        
        // Verify types are present
        assert!(action_tree["types"].is_object());
        let types = action_tree["types"].as_object().unwrap();
        assert!(types.contains_key("WeatherConfig"));
        assert!(types.contains_key("CustomWeatherResponse"));
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
    async fn test_execute_action_create_do_droplet() {
        dotenv::dotenv().ok();

        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing action for droplet creation
        let action_ref = "starthubhq/do-create-droplet:0.0.1";
        
        // Read test parameters from environment variables with defaults
        let api_token = std::env::var("DO_API_TOKEN")
            .unwrap_or_else(|_| "".to_string());
        let name = std::env::var("DO_DROPLET_NAME")
            .unwrap_or_else(|_| "test-droplet".to_string());
        let region = std::env::var("DO_DROPLET_REGION")
            .unwrap_or_else(|_| "nyc1".to_string());
        let size = std::env::var("DO_DROPLET_SIZE")
            .unwrap_or_else(|_| "s-1vcpu-1gb".to_string());
        let image = std::env::var("DO_DROPLET_IMAGE")
            .unwrap_or_else(|_| "ubuntu-20-04-x64".to_string());
        let ssh_keys = std::env::var("DO_DROPLET_SSH_KEYS")
            .unwrap_or_else(|_| "".to_string());
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
        let _ssh_keys_array: Vec<String> = if ssh_keys.is_empty() {
            vec![]
        } else {
            ssh_keys.split(',').map(|s| s.trim().to_string()).collect()
        };
        
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
        assert!(result.is_ok(), "execute_action should succeed for valid action_ref and inputs");
    }

    #[tokio::test]
    async fn test_execute_action_create_do_ssh_key_from_file() {
        dotenv::dotenv().ok();

        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing action for SSH key creation from file
        let action_ref = "starthubhq/do-create-ssh-key:0.0.1";
        
        // Read test parameters from environment variables with defaults
        let api_token = std::env::var("DO_API_TOKEN")
            .unwrap_or_else(|_| "".to_string());
        let name = std::env::var("DO_SSH_KEY_NAME")
            .unwrap_or_else(|_| "test-ssh-key-from-file".to_string());
        let ssh_key_file_path = std::env::var("DO_SSH_KEY_FILE_PATH")
            .unwrap_or_else(|_| "/tmp/test_ssh_key.pub".to_string());
        
        // Create a temporary SSH key file for testing if it doesn't exist
        if !std::path::Path::new(&ssh_key_file_path).exists() {
            let test_public_key = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQC7vbqajDhA test@example.com";
            if let Err(e) = std::fs::write(&ssh_key_file_path, test_public_key) {
                println!("Warning: Could not create test SSH key file at {}: {}", ssh_key_file_path, e);
            }
        }
        
        let inputs = vec![
            json!({
                "api_token": api_token,
                "name": name,
                "ssh_key_file_path": ssh_key_file_path
            })
        ];
        
        println!("inputs: {:#?}", inputs);
        let result = engine.execute_action(action_ref, inputs).await;
        
        println!("result: {:#?}", result);
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid action_ref and inputs with file path");
    }

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
        
        let inputs = vec![
            json!({
                "api_token": api_token,
                "droplet_id": droplet_id
            })
        ];
        
        println!("Testing do-get-droplet with inputs: {:#?}", inputs);
        let result = engine.execute_action(action_ref, inputs).await;
        
        println!("do-get-droplet test result: {:#?}", result);
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid do-get-droplet action_ref and inputs");
        
        let action_tree = result.unwrap();
        
        // Verify the action structure
        assert_eq!(action_tree["name"], "do-get-droplet");
        assert_eq!(action_tree["kind"], "composition");
        assert_eq!(action_tree["uses"], action_ref);
        
        // Verify inputs
        assert!(action_tree["inputs"].is_array());
        let inputs_array = action_tree["inputs"].as_array().unwrap();
        assert_eq!(inputs_array.len(), 1);
        let input = &inputs_array[0];
        assert_eq!(input["name"], "droplet_config");
        assert_eq!(input["type"], "DigitalOceanDropletGetConfig");
        
        // Verify outputs
        assert!(action_tree["outputs"].is_array());
        let outputs_array = action_tree["outputs"].as_array().unwrap();
        assert_eq!(outputs_array.len(), 1);
        let output = &outputs_array[0];
        assert_eq!(output["name"], "droplet");
        assert_eq!(output["type"], "DigitalOceanDroplet");
        
        // Execution order is now determined dynamically at runtime
        
        // Verify types are present
        assert!(action_tree["types"].is_object());
        let types = action_tree["types"].as_object().unwrap();
        assert!(types.contains_key("DigitalOceanDropletGetConfig"));
        assert!(types.contains_key("DigitalOceanDroplet"));
        
        // Verify permissions
        assert!(action_tree["permissions"].is_object());
        let permissions = action_tree["permissions"].as_object().unwrap();
        assert!(permissions.contains_key("net"));
        let net_permissions = permissions["net"].as_array().unwrap();
        assert!(net_permissions.contains(&json!("http")));
        assert!(net_permissions.contains(&json!("https")));
    }

    #[tokio::test]
    async fn test_execute_action_std_read_file() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing the std/read-file action
        let action_ref = "std/read-file:0.0.1";
        
        // Test with file path parameter
        let inputs = vec![
            json!("/Users/tommaso/Desktop/test.txt")
        ];
        
        println!("Testing std/read-file with inputs: {:#?}", inputs);
        let result = engine.execute_action(action_ref, inputs).await;
        
        println!("std/read-file test result: {:#?}", result);
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid std/read-file action_ref and inputs");
        
        let action_tree = result.unwrap();
        
        // Verify the action structure
        assert_eq!(action_tree["name"], "read-file");
        assert_eq!(action_tree["kind"], "wasm");
        assert_eq!(action_tree["uses"], action_ref);
    }

    #[tokio::test]
    async fn test_execute_action_sleep() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing the sleep action directly
        let action_ref = "std/sleep:0.0.1";
        
        // Test with two inputs: seconds and ignored value
        let inputs = vec![
            json!(2.5),  // seconds
            json!("ignored_value")  // second input that will be ignored
        ];
        
        println!("Testing sleep action with inputs: {:#?}", inputs);
        let result = engine.execute_action(action_ref, inputs).await;
        
        println!("Sleep test result: {:#?}", result);
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid sleep action_ref and inputs");
        
        let action_tree = result.unwrap();
        
        // Verify the action structure
        assert_eq!(action_tree["name"], "sleep");
        assert_eq!(action_tree["kind"], "wasm");
        assert_eq!(action_tree["uses"], action_ref);
    }

    #[tokio::test]
    async fn test_execute_action_base64_to_text() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing the base64-to-text action directly
        let action_ref = "starthubhq/base64-to-text:0.0.1";
        
        // Test with base64 encoded "Hello World" and ignored value
        let inputs = vec![
            json!("SGVsbG8gV29ybGQ="),  // base64 encoded "Hello World"
            json!("ignored_value")  // second input that will be ignored
        ];
        
        println!("Testing base64-to-text action with inputs: {:#?}", inputs);
        let result = engine.execute_action(action_ref, inputs).await;
        
        println!("Base64-to-text test result: {:#?}", result);
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid base64-to-text action_ref and inputs");
        
        let action_tree = result.unwrap();
        
        // Verify the action structure
        assert_eq!(action_tree["name"], "base64-to-text");
        assert_eq!(action_tree["kind"], "wasm");
        assert_eq!(action_tree["uses"], action_ref);
        
        // Verify that the action has the expected inputs and outputs
        assert!(action_tree["inputs"].is_array());
        let inputs_array = action_tree["inputs"].as_array().unwrap();
        
        // Check first input (base64_string)
        let first_input = &inputs_array[0];
        assert_eq!(first_input["name"], "base64_string");
        assert_eq!(first_input["type"], "string");
        assert_eq!(first_input["required"], true);
        
        // Verify outputs
        assert!(action_tree["outputs"].is_array());
        let outputs_array = action_tree["outputs"].as_array().unwrap();
        assert_eq!(outputs_array.len(), 1);
        
        let output = &outputs_array[0];
        assert_eq!(output["name"], "text");
        assert_eq!(output["type"], "string");
    }

    #[tokio::test]
    async fn test_execute_action_file_to_string() {
        // Create a mock ExecutionEngine
        let mut engine = ExecutionEngine::new();
        
        // Test executing the file-to-string composition
        let action_ref = "starthubhq/file-to-string:0.0.1";
        
        // Test with file path parameter
        let inputs = vec![
            json!({
                "file_path": "/Users/tommaso/Desktop/test.txt"
            })
        ];
        
        println!("Testing file-to-string composition with inputs: {:#?}", inputs);
        let result = engine.execute_action(action_ref, inputs).await;
        
        println!("File-to-string test result: {:#?}", result);
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid file-to-string action_ref and inputs");
        
        let action_tree = result.unwrap();
        
        // Verify the action structure
        assert_eq!(action_tree["name"], "file-to-string");
        assert_eq!(action_tree["kind"], "composition");
        assert_eq!(action_tree["uses"], action_ref);
        
        // Verify inputs
        assert!(action_tree["inputs"].is_array());
        let inputs_array = action_tree["inputs"].as_array().unwrap();
        assert_eq!(inputs_array.len(), 1);
        let input = &inputs_array[0];
        assert_eq!(input["name"], "file_config");
        assert_eq!(input["type"], "FileConfig");
        
        // Verify outputs
        assert!(action_tree["outputs"].is_array());
        let outputs_array = action_tree["outputs"].as_array().unwrap();
        assert_eq!(outputs_array.len(), 1);
        let output = &outputs_array[0];
        assert_eq!(output["name"], "content");
        assert_eq!(output["type"], "string");
        
        // Execution order is now determined dynamically at runtime
        
        // Verify types are present
        assert!(action_tree["types"].is_object());
        let types = action_tree["types"].as_object().unwrap();
        assert!(types.contains_key("FileConfig"));
        
        // Verify permissions
        assert!(action_tree["permissions"].is_object());
        let permissions = action_tree["permissions"].as_object().unwrap();
        assert!(permissions.contains_key("fs"));
        let fs_permissions = permissions["fs"].as_array().unwrap();
        assert!(fs_permissions.contains(&json!("read")));
    }
    
    #[test]
    fn test_execution_engine_new() {
        // Test creating a new ExecutionEngine
        let engine = ExecutionEngine::new();
        
        // Verify the cache directory is set correctly
        let expected_cache_dir = dirs::cache_dir()
            .unwrap_or(std::env::temp_dir())
            .join("starthub/oci");
        
        assert_eq!(engine.cache_dir, expected_cache_dir);
    }

    #[tokio::test]
    async fn test_build_action_tree() {
        // Create a mock ExecutionEngine
        let engine = ExecutionEngine::new();
        
        // Test building action tree for the coordinates action
        let action_ref = "starthubhq/get-weather-by-location-name:0.0.1";
        let result = engine.build_action_tree(action_ref, None).await;
        
        // The test should succeed
        assert!(result.is_ok(), "build_action_tree should succeed for valid action_ref");
        
        let action_tree = result.unwrap();
        
        // Verify the root action structure
        assert_eq!(action_tree.name, "get-weather-by-location-name");
        assert_eq!(action_tree.kind, "composition");
        assert_eq!(action_tree.uses, action_ref);
        assert!(action_tree.parent_action.is_none());
        
        // Verify inputs
        assert_eq!(action_tree.inputs.len(), 1);
        let input = &action_tree.inputs[0];
        assert_eq!(input.name, "weather_config");
        assert_eq!(input.r#type, "WeatherConfig");
        
        // Verify outputs
        assert_eq!(action_tree.outputs.len(), 1);
        let output = &action_tree.outputs[0];
        assert_eq!(output.name, "response");
        assert_eq!(output.r#type, "CustomWeatherResponse");
        
        // Execution order is now determined dynamically at runtime
        
        // Verify types are present
        assert!(action_tree.types.is_some());
        let types = action_tree.types.as_ref().unwrap();
        assert!(types.contains_key("WeatherConfig"));
        assert!(types.contains_key("CustomWeatherResponse"));
    }

    




    #[tokio::test]
    async fn test_fetch_manifest() {
        // Create a mock ExecutionEngine
        let engine = ExecutionEngine::new();
        
        // Test fetching a real manifest from the starthub API
        let action_ref = "starthubhq/get-weather-by-location-name:0.0.1";
        let result = engine.fetch_manifest(action_ref).await;
        
        // The test should succeed and return a valid manifest
        assert!(result.is_ok(), "fetch_manifest should succeed for valid action_ref");
        
        let manifest = result.unwrap();
        
        // Verify the manifest has the expected structure
        assert_eq!(manifest.name, "get-weather-by-location-name");
        assert_eq!(manifest.version, "0.0.1");
        assert_eq!(manifest.kind, Some(ShKind::Composition));
        
        // Verify it has inputs
        assert!(manifest.inputs.is_array());
        assert!(manifest.inputs.as_array().unwrap().len() > 0);
        
        // Verify it has outputs
        assert!(manifest.outputs.is_array());
        assert!(manifest.outputs.as_array().unwrap().len() > 0);
    }

    #[tokio::test]
    async fn test_fetch_manifest_invalid_ref() {
        // Create a mock ExecutionEngine
        let engine = ExecutionEngine::new();
        
        // Test fetching a non-existent manifest
        let action_ref = "starthubhq/non-existent-action:1.0.0";
        let result = engine.fetch_manifest(action_ref).await;
        
        // The test should fail for invalid action_ref
        assert!(result.is_err(), "fetch_manifest should fail for invalid action_ref");
    }

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

    #[tokio::test]
    async fn test_run_action_tree_wasm_early_return() {
        // Test that WASM actions return early without processing
        let mut engine = ExecutionEngine::new();
        
        let mut wasm_action = ShAction {
            id: "test-wasm".to_string(),
            name: "test-wasm".to_string(),
            kind: "wasm".to_string(),
            uses: "test:wasm".to_string(),
            inputs: vec![],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            flow_control: false,
            types: None,
            mirrors: vec![],
            permissions: None,
        };
        
        let inputs = vec![json!({"test": "data"})];
        let result = engine.run_action_tree(&mut wasm_action, &inputs, &HashMap::new()).await;
        
        // Should succeed and return early without processing
        assert!(result.is_ok(), "run_action_tree should succeed for wasm action");
    }

    #[test]
    fn test_resolve_template() {
        let engine = ExecutionEngine::new();
        
        // Test case 1: String template
        let template_string = json!("Hello {{inputs[0].name}}");
        let parent_inputs = vec![json!({"name": "World"})];
        let executed_steps = HashMap::new();
        
        let result = engine.interpolate(&template_string, &parent_inputs, &executed_steps);
        assert!(result.is_ok(), "resolve_template should succeed for string template");
        let resolved = result.unwrap();
        println!("Resolved: {:#?}", resolved);
        assert_eq!(resolved, json!("Hello {{inputs[0].name}}")); // Currently returns as-is since resolve_template_string is not implemented
        
        // Test case 2: Object template
        let template_object = json!({
            "name": "{{inputs[0].name}}",
            "age": 25,
            "nested": {
                "city": "{{inputs[0].city}}"
            }
        });
        let parent_inputs_obj = vec![json!({"name": "Alice", "city": "New York"})];
        
        let result = engine.interpolate(&template_object, &parent_inputs_obj, &executed_steps);
        assert!(result.is_ok(), "resolve_template should succeed for object template");
        let resolved = result.unwrap();
        assert_eq!(resolved["name"], json!("{{inputs[0].name}}")); // Currently returns as-is
        assert_eq!(resolved["age"], json!(25)); // Non-template values preserved
        assert_eq!(resolved["nested"]["city"], json!("{{inputs[0].city}}")); // Currently returns as-is
        
        // Test case 3: Array template
        let template_array = json!([
            "{{inputs[0].item1}}",
            "{{inputs[0].item2}}",
            "static_value"
        ]);
        let parent_inputs_arr = vec![json!({"item1": "value1", "item2": "value2"})];
        
        let result = engine.interpolate(&template_array, &parent_inputs_arr, &executed_steps);
        assert!(result.is_ok(), "resolve_template should succeed for array template");
        let resolved = result.unwrap();
        assert!(resolved.is_array());
        let resolved_array = resolved.as_array().unwrap();
        assert_eq!(resolved_array.len(), 3);
        assert_eq!(resolved_array[0], json!("{{inputs[0].item1}}")); // Currently returns as-is
        assert_eq!(resolved_array[1], json!("{{inputs[0].item2}}")); // Currently returns as-is
        assert_eq!(resolved_array[2], json!("static_value")); // Non-template values preserved
        
        // Test case 4: Non-string/non-object/non-array template (should be returned as-is)
        let template_number = json!(42);
        let result = engine.interpolate(&template_number, &parent_inputs, &executed_steps);
        assert!(result.is_ok(), "resolve_template should succeed for number template");
        let resolved = result.unwrap();
        assert_eq!(resolved, json!(42));
        
        // Test case 5: Null template
        let template_null = json!(null);
        let result = engine.interpolate(&template_null, &parent_inputs, &executed_steps);
        assert!(result.is_ok(), "resolve_template should succeed for null template");
        let resolved = result.unwrap();
        assert_eq!(resolved, json!(null));
        
        // Test case 6: Boolean template
        let template_bool = json!(true);
        let result = engine.interpolate(&template_bool, &parent_inputs, &executed_steps);
        assert!(result.is_ok(), "resolve_template should succeed for boolean template");
        let resolved = result.unwrap();
        assert_eq!(resolved, json!(true));
    }

    #[test]
    fn test_instantiate_primitive_types() {
        let engine = ExecutionEngine::new();
        
        // Test case 1: String type
        let io_definitions = vec![
            ShIO {
                name: "test_string".to_string(),
                r#type: "string".to_string(),
                template: json!("test"),
                value: None,
                required: true,
            }
        ];
        let io_values = vec![json!("hello world")];
        let types = None;
        
        let result = engine.cast(&types, &io_definitions, &io_values);
        assert!(result.is_ok(), "instantiate should succeed for string type");
        let instantiated = result.unwrap();
        assert_eq!(instantiated.len(), 1);
        assert_eq!(instantiated[0], json!("hello world"));
        
        // Test case 2: Boolean type
        let io_definitions = vec![
            ShIO {
                name: "test_bool".to_string(),
                r#type: "bool".to_string(),
                template: json!(true),
                value: None,
                required: true,
            }
        ];
        let io_values = vec![json!(false)];
        
        let result = engine.cast(&types, &io_definitions, &io_values);
        assert!(result.is_ok(), "instantiate should succeed for bool type");
        let instantiated = result.unwrap();
        assert_eq!(instantiated.len(), 1);
        assert_eq!(instantiated[0], json!(false));
        
        // Test case 3: Number type
        let io_definitions = vec![
            ShIO {
                name: "test_number".to_string(),
                r#type: "number".to_string(),
                template: json!(42),
                value: None,
                required: true,
            }
        ];
        let io_values = vec![json!(3.14)];
        
        let result = engine.cast(&types, &io_definitions, &io_values);
        assert!(result.is_ok(), "instantiate should succeed for number type");
        let instantiated = result.unwrap();
        assert_eq!(instantiated.len(), 1);
        assert_eq!(instantiated[0], json!(3.14));
        
        // Test case 4: Object type
        let io_definitions = vec![
            ShIO {
                name: "test_object".to_string(),
                r#type: "object".to_string(),
                template: json!({}),
                value: None,
                required: true,
            }
        ];
        let io_values = vec![json!({"key": "value", "nested": {"inner": 123}})];
        
        let result = engine.cast(&types, &io_definitions, &io_values);
        assert!(result.is_ok(), "instantiate should succeed for object type");
        let instantiated = result.unwrap();
        assert_eq!(instantiated.len(), 1);
        assert_eq!(instantiated[0], json!({"key": "value", "nested": {"inner": 123}}));
    }

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

    #[test]
    fn test_instantiate_empty_inputs() {
        let engine = ExecutionEngine::new();
        
        let io_definitions = vec![];
        let io_values = vec![];
        let types = None;
        
        let result = engine.cast(&types, &io_definitions, &io_values);
        assert!(result.is_ok(), "instantiate should succeed for empty inputs");
        let instantiated = result.unwrap();
        assert_eq!(instantiated.len(), 0);
    }

    #[test]
    #[should_panic(expected = "called `Option::unwrap()` on a `None` value")]
    fn test_instantiate_mismatched_lengths() {
        let engine = ExecutionEngine::new();
        
        let io_definitions = vec![
            ShIO {
                name: "test1".to_string(),
                r#type: "string".to_string(),
                template: json!(""),
                value: None,
                required: true,
            },
            ShIO {
                name: "test2".to_string(),
                r#type: "string".to_string(),
                template: json!(""),
                value: None,
                required: true,
            }
        ];
        let io_values = vec![json!("only_one_value")]; // Only one value for two definitions
        
        let _result = engine.cast(&None, &io_definitions, &io_values);
        // This should panic due to unwrap() in the method when trying to access the second value
    }



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
        
        println!("http-get-wasm test result: {:#?}", result);
        // The test should succeed
        assert!(result.is_ok(), "execute_action should succeed for valid http-get-wasm action_ref and inputs");
        
        let action_tree = result.unwrap();
        
        // Verify the action structure
        assert_eq!(action_tree["name"], "http-get-wasm");
        assert_eq!(action_tree["kind"], "wasm");
        assert_eq!(action_tree["uses"], action_ref);
        
        // Verify that the action has the expected inputs and outputs
        assert!(action_tree["inputs"].is_array());
        let inputs_array = action_tree["inputs"].as_array().unwrap();
        assert_eq!(inputs_array.len(), 2);
        
        // Check first input (url)
        let first_input = &inputs_array[0];
        assert_eq!(first_input["name"], "url");
        assert_eq!(first_input["type"], "string");
        assert_eq!(first_input["required"], true);
        
        // Check second input (headers)
        let second_input = &inputs_array[1];
        assert_eq!(second_input["name"], "headers");
        assert_eq!(second_input["type"], "HttpHeaders");
        assert_eq!(second_input["required"], false);
        
        // Verify outputs
        assert!(action_tree["outputs"].is_array());
        let outputs_array = action_tree["outputs"].as_array().unwrap();
        assert_eq!(outputs_array.len(), 1);
        
        let output = &outputs_array[0];
        assert_eq!(output["name"], "response");
        assert_eq!(output["type"], "HttpResponse");
        
        // Verify types are present
        assert!(action_tree["types"].is_object());
        let types = action_tree["types"].as_object().unwrap();
        assert!(types.contains_key("HttpHeaders"));
        assert!(types.contains_key("HttpResponse"));
        
        // Verify permissions
        assert!(action_tree["permissions"].is_object());
        let permissions = action_tree["permissions"].as_object().unwrap();
        assert!(permissions.contains_key("net"));
        let net_permissions = permissions["net"].as_array().unwrap();
        assert!(net_permissions.contains(&json!("http")));
        assert!(net_permissions.contains(&json!("https")));
    }
    
    
}