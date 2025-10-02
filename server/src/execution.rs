use anyhow::{Result, bail};
use jsonschema::JSONSchema;
use serde_json::Value;
use std::collections::HashMap;
use dirs;
use reqwest::{self};
use petgraph::Graph;
use petgraph::algo::toposort;
use tokio::process::Command as TokioCommand;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use which;
use zip::ZipArchive;
use tokio::sync::broadcast;

use crate::models::{ShManifest, ShKind, ShIO, ShAction};

// Constants
const STARTHUB_API_BASE_URL: &str = "https://api.starthub.so";
const STARTHUB_STORAGE_PATH: &str = "/storage/v1/object/public/artifacts";
const STARTHUB_MANIFEST_FILENAME: &str = "starthub-lock.json";
pub struct ExecutionEngine {
    cache_dir: std::path::PathBuf,
    ws_sender: Option<broadcast::Sender<String>>,
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
        
        Self {
            cache_dir,
            ws_sender: None,
        }
    }

    pub fn set_ws_sender(&mut self, ws_sender: broadcast::Sender<String>) {
        self.ws_sender = Some(ws_sender);
    }

    // Logging utility method
    fn log(&self, level: &str, message: &str, action_id: Option<&str>) {
        if let Some(sender) = &self.ws_sender {
            let log_msg = serde_json::json!({
                "type": "log",
                "level": level,
                "message": message,
                "action_id": action_id,
                "timestamp": chrono::Utc::now().to_rfc3339()
            });
            
            if let Ok(msg_str) = serde_json::to_string(&log_msg) {
                let _ = sender.send(msg_str);
            }
        }
    }

    // Convenience logging methods
    fn log_info(&self, message: &str, action_id: Option<&str>) {
        self.log("info", message, action_id);
    }

    fn log_error(&self, message: &str, action_id: Option<&str>) {
        self.log("error", message, action_id);
    }


    fn log_success(&self, message: &str, action_id: Option<&str>) {
        self.log("success", message, action_id);
    }

    // Main flow
    pub async fn execute_action(&self, action_ref: &str, inputs: Vec<Value>) -> Result<Value> {
        self.log_info(&format!("Starting execution of action: {}", action_ref), None);
        
        // Ensure cache directory exists before starting execution.
        // It should already exist, but just in case.
        if let Err(e) = std::fs::create_dir_all(&self.cache_dir) {
            self.log_error(&format!("Failed to create cache directory: {}", e), None);
            return Err(anyhow::anyhow!("Failed to create cache directory: {}", e));
        }
        
        // 1. Build the action tree
        self.log_info("Building action tree...", None);
        let mut root_action = self.build_action_tree(
            action_ref,         // Action reference to download
            None,               // No parent action ID (root)
        ).await?;     
        self.log_success("Action tree built successfully", Some(&root_action.id));

        self.log_info("Executing action tree...", Some(&root_action.id));
        self.run_action_tree(&mut root_action,
            &inputs, &HashMap::new()).await?;
        self.log_success("Action execution completed", Some(&root_action.id));

        // Return the action tree (no execution)
        Ok(serde_json::to_value(root_action)?)
    }

    async fn run_action_tree(&self,
        action_state: &mut ShAction,
        parent_inputs: &Vec<Value>,
        executed_sibling_steps: &HashMap<String, ShAction>) -> Result<ShAction> {
        
        // 1) Instantiate the inputs according to the types specified
        let instantiated_inputs: Vec<Value> = self.instantiate(
            &action_state.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()), 
            &action_state.inputs,
            &parent_inputs
        )?;

        // 2) Assign the instantiated inputs to the action state inputs
        for (index, input) in action_state.inputs.iter_mut().enumerate() {
            if let Some(resolved_input) = instantiated_inputs.get(index) {
                input.value = Some(resolved_input.clone());
            }
        }

        // println!("instantiated_inputs: {:#?}", instantiated_inputs);
        // println!("action_state: {:#?}", action_state);

        // Base condition
        if action_state.kind == "wasm" || action_state.kind == "docker" {
            self.log_info(&format!("Executing {} step: {}", action_state.kind, action_state.name), Some(&action_state.id));
            
            // Serialize the instantiated inputs
            let inputs_value = serde_json::to_value(&instantiated_inputs)?;
            let result = self.run_wasm_step(action_state, None, &inputs_value).await?;
            
            self.log_success(&format!("{} step completed: {}", action_state.kind, action_state.name), Some(&action_state.id));

            // Parse the result into a vector of JSON objects
            let json_objects: Vec<Value> = if result.is_empty() {
                Vec::new()
            } else {
                if let Some(first_result) = result.first() {
                    if let Some(array) = first_result.as_array() {
                        array.iter().map(|item| Self::parse_json_strings_recursively(item.clone())).collect()
                    } else {
                        vec![Self::parse_json_strings_recursively(first_result.clone())]
                    }
                } else {
                    Vec::new()
                }
            };

            let mut action_state_clone = action_state.clone();
            
            let instantiated_outputs: Vec<Value> = self.instantiate(
                &action_state.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()), 
                &action_state.outputs,
                &json_objects
            )?;

            // For every output, want to assign the value of the corresponding resolved
            // output to its value field.
            for (index, output) in action_state_clone.outputs.iter_mut().enumerate() {
                if let Some(resolved_output) = instantiated_outputs.get(index) {
                    output.value = Some(resolved_output.clone());
                }
            }

            return Ok(action_state_clone);
        }

        // Track executed steps as we go
        let mut local_executed_steps = executed_sibling_steps.clone();
        
        // Run the action tree recursively - DFS
        for step_id in &action_state.execution_order {
            if let Some(step) = action_state.steps.get_mut(step_id) {
                println!("step_id: {:#?}", step_id);
                // For each step, we need to use the inputs and types field
                // of the step to generate a completely new object with that structure.
                
                // The inputs field determines not only the order of the inputs, but
                // also the structure of the input, along with how the inputs from the
                // current action or sibling need to be injected into each child step input.

                // 2) Generate the inputs object that are going to be passed to the next recursion.
                // Resolve inputs for this step using the same logic as the main resolve_inputs function
                let resolved_inputs_to_inject_into_child_step = self.resolve_into_inputs(
                    &step.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()),
                    &step.inputs,
                    &instantiated_inputs,  // Inject inputs from current action into the step
                    &local_executed_steps
                )?;
                
                // Execute the step with its own raw inputs, parent inputs for template resolution, and executed steps
                let processed_child = Box::pin(self.run_action_tree(
                    step, 
                    &resolved_inputs_to_inject_into_child_step,  // Parent's resolved inputs for template resolution
                    &local_executed_steps
                )).await?;
                
                // Given the step name, assing the processed child to the step
                let processed_child_clone = processed_child.clone();
                action_state.steps.insert(step_id.clone(), processed_child);
                
                // Add the executed steps to the executed siblings, so in the
                // next iteration the template resolution can pick up the outputs
                // of the executed steps.
                local_executed_steps.insert(step_id.clone(), processed_child_clone);
            }
        }

        // If we got here, it means that we have executed all the steps in the action tree.
        // Now we need to aggregate the outputs back at the higher level.
        let resolved_outputs = self.resolve_into_outputs(
            &action_state.inputs,
            &action_state.outputs,
            &action_state.steps
        )?;

        // valueFor every output, want to assign the value of the corresponding resolved
        // output to its value field.
        for (index, output) in action_state.outputs.iter_mut().enumerate() {
            if let Some(resolved_output) = resolved_outputs.get(index) {
                output.value = Some(resolved_output.clone());
            }
        }

        return Ok(action_state.clone());
    }

    fn parse_json_strings_recursively(value: Value) -> Value {
        match value {
            Value::Object(mut obj) => {
                for (_, val) in obj.iter_mut() {
                    *val = Self::parse_json_strings_recursively(val.clone());
                }
                Value::Object(obj)
            },
            Value::Array(arr) => {
                Value::Array(arr.into_iter().map(Self::parse_json_strings_recursively).collect())
            },
            Value::String(s) => {
                // Try to parse as JSON
                if let Ok(parsed) = serde_json::from_str::<Value>(&s) {
                    Self::parse_json_strings_recursively(parsed)
                } else {
                    Value::String(s)
                }
            },
            _ => value
        }
    }

    fn instantiate(&self, types: &Option<HashMap<String, Value>>, 
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

    // We need both input and output definitions, as long as all steps, since the
    // output values might come from the inputs or the outputs of the steps.
    fn resolve_into_outputs(&self,
        inputs: &Vec<ShIO>, //Contains both template and values of the inputs
        output_definitions: &Vec<ShIO>, //Only contains the template of the outputs
        steps: &HashMap<String, ShAction>) -> Result<Vec<Value>> {        
        // We extract the types from the action state
        let mut resolved_outputs: Vec<Value> = Vec::new();

        // println!("types: {:#?}", types);
        // println!("io_definitions: {:#?}", io_definitions);
        // println!("io_values: {:#?}", io_values);
        // println!("executed_sibling_steps: {:#?}", executed_sibling_steps);

        // We want to create a vector of input by extracting the value from the inputs vector.
        // This is because the outputs might be fetching values from the inputs directly.
        let inputs_values = inputs.iter().map(|input| input.value.clone().unwrap_or(Value::Null)).collect::<Vec<_>>();

        // For every value, find its corresponding input by index
        for (index, _output) in output_definitions.iter().enumerate() {
            if let Some(output) = output_definitions.get(index) {                
                // First, resolve the template to get the actual input value
                let interpolated_template = self.interpolate(
                    &output.template, 
                    &inputs_values, 
                    steps
                )?;

                resolved_outputs.push(interpolated_template);
            }
        }

        Ok(resolved_outputs)
    }

    

    // Given a key value types object, a list of input definitions,
    // a list of input values and a list of executed sibling steps,
    // it returns a list of type-checked, resolved inputs.
    fn resolve_into_inputs(&self,
        _types: &Option<HashMap<String, Value>>, 
        io_definitions: &Vec<ShIO>,
        io_values: &Vec<Value>,
        executed_sibling_steps: &HashMap<String, ShAction>) -> Result<Vec<Value>> {        
        // We extract the types from the action state
        let mut resolved_inputs: Vec<Value> = Vec::new();

        // For every value, find its corresponding input by index
        for (index, _input) in io_definitions.iter().enumerate() {
            if let Some(input) = io_definitions.get(index) {
                // First, resolve the template to get the actual input value
                let interpolated_template = self.interpolate(
                    &input.template, 
                    io_values, 
                    executed_sibling_steps
                )?;

                resolved_inputs.push(interpolated_template);
            }
        }

        Ok(resolved_inputs)
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
                Ok(Value::String(resolved))
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
                                }
                            }
                        }
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
            // Initially empty execution order
            execution_order: Vec::new(),
            // Initially empty types
            types: if manifest.types.is_empty() { None } else { Some(manifest.types.clone().into_iter().collect()) },
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
        // 5. Topologically sort steps based on dependencies
        let sorted_step_ids = self.topological_sort_composition_steps(&action_state.steps).await?;

        // 6. Add the sorted step IDs to the execution order field
        for step_id in sorted_step_ids {
            action_state.execution_order.push(step_id);
        }

        return Ok(action_state);
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

    async fn topological_sort_composition_steps(&self, steps: &HashMap<String, ShAction>) -> Result<Vec<String>> {
        // Build dependency graph using petgraph
        let mut graph = Graph::<String, ()>::new();
        let mut node_map = HashMap::new();
        
        // Nodes
        // Add all child actions as nodes
        for (child_id, _) in steps {
            let node = graph.add_node(child_id.clone());
            node_map.insert(child_id.clone(), node);
        }
        
        // Edges
        // Analyze dependencies by looking at template variables in child action inputs
        for (child_id, child_action) in steps {
            // Every composite action contains, in its child steps, the mapping of how they use
            // either the inputs or other steps to get information to use.

            // For each input in the child action
            // find the template dependencies
            for input in child_action.inputs.clone() {
                let step_deps = self.find_sibling_dependencies(&serde_json::to_value(&input.template)?, steps)?;
                
                for dep in step_deps {
                    if let (Some(&current_node), Some(&dep_node)) = (node_map.get(child_id), node_map.get(&dep)) {
                        graph.add_edge(dep_node, current_node, ());
                    }
                }
            }
        }
        
        // println!("Graph DOT format:");
        // println!("{:?}", petgraph::dot::Dot::new(&graph));
        // Perform topological sort
        let sorted_nodes = toposort(&graph, None)
            .map_err(|_| anyhow::anyhow!("Circular dependency detected in composition steps"))?;
        
        // Convert back to step IDs
        let mut sorted_step_ids = Vec::new();
        for node in sorted_nodes {
            let step_id = &graph[node];
            sorted_step_ids.push(step_id.clone());
        }
        
        Ok(sorted_step_ids)
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


    async fn run_wasm_step(
        &self,
        action: &mut ShAction,
        pipeline_workdir: Option<&str>,
        inputs: &Value,
    ) -> Result<Vec<Value>> {
        if which::which("wasmtime").is_err() {
            self.log_error("wasmtime not found in PATH", Some(&action.id));
            bail!("wasmtime not found in PATH");
        }

        self.log_info(&format!("Downloading WASM module: {}", action.uses), Some(&action.id));
        // For now, we'll create a simple implementation that downloads the WASM file
        // In a real implementation, this would download from the registry
        let module_path = self.download_wasm(&action.uses).await?;
        self.log_success(&format!("WASM module downloaded: {:?}", module_path), Some(&action.id));
        
        // Verify the WASM file exists and is readable
        if !module_path.exists() {
            return Err(anyhow::anyhow!("WASM file not found at: {:?}", module_path));
        }
        
        // Check if the file is readable
        if let Err(e) = std::fs::metadata(&module_path) {
            return Err(anyhow::anyhow!("WASM file not accessible at {:?}: {}", module_path, e));
        }

        // build stdin payload - use the pre-built parameters
        let input_json = serde_json::to_string(inputs)?;

        self.log_info(&format!("Running WASM file: {:?}", module_path), Some(&action.id));
        self.log_info(&format!("Input: {}", input_json), Some(&action.id));
        
        // Construct command
        let mut cmd = TokioCommand::new("wasmtime");
        cmd.arg("-S").arg("http");
        cmd.arg(&module_path);

        // working dir if absolute
        if let Some(wd) = pipeline_workdir {
            if wd.starts_with('/') { 
                cmd.current_dir(wd); 
            }
        }

        // spawn with piped stdio
        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn wasmtime for step {}: {}", action.id, e))?;

        // feed stdin JSON
        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(input_json.as_bytes()).await?;
        }
        drop(child.stdin.take());

        // pump stdout/stderr and collect patches
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let mut out_reader = BufReader::new(stdout);
        let mut err_reader = BufReader::new(stderr);

        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();

        let pump_out = tokio::spawn(async move {
            let mut line = String::new();
            while out_reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                // Try to parse the line directly as JSON
                if let Ok(v) = serde_json::from_str::<Value>(line.trim()) {
                    let _ = tx.send(v);
                }
                line.clear();
            }
        });

        let pump_err = tokio::spawn(async move {
            let mut line = String::new();
            while err_reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                // Just consume stderr for now
                line.clear();
            }
        });

        let status = child.wait().await?;
        let _ = pump_out.await;
        let _ = pump_err.await;

        if !status.success() {
            self.log_error(&format!("WASM execution failed with status: {}", status), Some(&action.id));
            bail!("step '{}' failed with {}", action.id, status);
        }
        
        self.log_success("WASM execution completed successfully", Some(&action.id));

        // Collect the first result from the WASM module
        let mut results = Vec::new();
        while let Ok(v) = rx.try_recv() { 
            results.push(v);
        }
        
        // The WASM module outputs a single JSON array, so we take the first result
        if results.is_empty() {
            // If no results, return an empty vector
            Ok(Vec::new())
        } else {
            // Take the first result and parse it as an array
            let first_result = &results[0];
            if let Some(array) = first_result.as_array() {
                Ok(array.clone())
            } else {
                // If it's not an array, wrap it in a single-element array
                Ok(vec![first_result.clone()])
            }
        }
    }

    async fn download_wasm(&self, action_ref: &str) -> Result<std::path::PathBuf> {
        println!("Downloading WASM file for action: {}", action_ref);
        // Construct the WASM file path in the cache directory with proper directory structure
        let url_path = action_ref.replace(":", "/");
        let wasm_dir = self.cache_dir.join(&url_path);
        let wasm_path = wasm_dir.join("artifact.wasm");
        
        // If the WASM file already exists, return it
        if wasm_path.exists() {
            println!("WASM file already exists at: {:?}", wasm_path);
            return Ok(wasm_path);
        }
        
        // Create the directory structure if it doesn't exist
        if let Err(e) = std::fs::create_dir_all(&wasm_dir) {
            return Err(anyhow::anyhow!("Failed to create directory {:?}: {}", wasm_dir, e));
        }
        
        // Download the artifact.zip file from the registry
        let storage_url = format!(
            "{}{}/{}/artifact.zip",
            STARTHUB_API_BASE_URL,
            STARTHUB_STORAGE_PATH,
            url_path
        );

        println!("Downloading artifact from: {}", storage_url);
        
        let client = reqwest::Client::new();
        let response = client.get(&storage_url).send().await?;
        
        if response.status().is_success() {
            let zip_bytes = response.bytes().await?;
            
            // Create a temporary file for the zip
            let temp_zip_path = wasm_dir.join("temp_artifact.zip");
            std::fs::write(&temp_zip_path, zip_bytes)?;
            
            // Extract the WASM file from the zip
            self.extract_wasm_from_zip(&temp_zip_path, &wasm_path).await?;
            
            // Clean up the temporary zip file
            std::fs::remove_file(&temp_zip_path)?;
            
            println!("WASM file extracted to: {:?}", wasm_path);
            Ok(wasm_path)
        } else {
            Err(anyhow::anyhow!("Failed to download artifact: {}", response.status()))
        }
    }

    async fn extract_wasm_from_zip(&self, zip_path: &std::path::Path, wasm_path: &std::path::Path) -> Result<()> {
        use std::fs::File;
        use std::io::Read;
        
        let file = File::open(zip_path)?;
        let mut archive = ZipArchive::new(file)?;
        
        // Find the .wasm file in the archive
        for i in 0..archive.len() {
            let file = archive.by_index(i)?;
            if file.name().ends_with(".wasm") {
                let mut wasm_content = Vec::new();
                let mut reader = std::io::BufReader::new(file);
                reader.read_to_end(&mut wasm_content)?;
                std::fs::write(wasm_path, wasm_content)?;
                return Ok(());
            }
        }
        
        Err(anyhow::anyhow!("No .wasm file found in the artifact zip"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    use serde_json::json;

    // TODO: test_find_template_dependencies to be fixed
    // #[test]
    // fn test_find_template_dependencies() {
    //     // Create a mock ExecutionEngine
    //     let engine = ExecutionEngine::new();
        
    //     // Create mock steps representing the weather composition scenario
    //     let mut steps = HashMap::new();
        
    //     // Step 1: get_coordinates - no dependencies
    //     let coordinates_id = uuid::Uuid::new_v4().to_string();
    //     let coordinates_id_clone = coordinates_id.clone();
    //     steps.insert("get_coordinates".to_string(), ShAction {
    //         id: coordinates_id,
    //         name: "get_coordinates".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "starthubhq/openweather-coordinates-by-location-name:0.0.1".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "open_weather_config".to_string(),
    //                 r#type: "OpenWeatherConfig".to_string(),
    //                 template: json!({
    //                     "location_name": "{{inputs[0].location_name}}",
    //                     "open_weather_api_key": "{{inputs[0].open_weather_api_key}}"
    //                 }),
    //                 value: None,
    //             }
    //         ],
    //         outputs: vec![
    //             ShIO {
    //                 name: "coordinates".to_string(),
    //                 r#type: "GeocodingResponse".to_string(),
    //                 template: json!({}),
    //                 value: None,
    //             }
    //         ],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         execution_order: vec![],
    //         types: None,
    //     });
        
    //     // Step 2: get_weather - depends on get_coordinates
    //     let weather_id = uuid::Uuid::new_v4().to_string();
    //     let weather_id_clone = weather_id.clone();
    //     steps.insert("get_weather".to_string(), ShAction {
    //         id: weather_id,
    //         name: "get_weather".to_string(),
    //         kind: "wasm".to_string(),
    //         uses: "starthubhq/openweather-current-weather:0.0.1".to_string(),
    //         inputs: vec![
    //             ShIO {
    //                 name: "weather_config".to_string(),
    //                 r#type: "WeatherConfig".to_string(),
    //                 template: json!({
    //                     "lat": "{{steps.get_coordinates.outputs[0].coordinates.lat}}",
    //                     "lon": "{{steps.get_coordinates.outputs[0].coordinates.lon}}",
    //                     "open_weather_api_key": "{{inputs.weather_config.open_weather_api_key}}"
    //                 }),
    //                 value: None,
    //             }
    //         ],
    //         outputs: vec![
    //             ShIO {
    //                 name: "weather".to_string(),
    //                 r#type: "WeatherResponse".to_string(),
    //                 template: json!({}),
    //                 value: None,
    //             }
    //         ],
    //         parent_action: None,
    //         steps: HashMap::new(),
    //         execution_order: vec![],
    //         types: None,
    //     });

    //     // Test case 1: Template that references get_coordinates step (like in the weather composition)
    //     let template_value = json!("{{steps.get_coordinates.outputs[0].coordinates.lat}}");
    //     let result = engine.find_template_dependencies(&template_value, &steps).unwrap();
    //     assert_eq!(result, vec![coordinates_id_clone.clone()]);

    //     // Test case 2: Template that references get_coordinates step for longitude
    //     let template_value = json!("{{steps.get_coordinates.outputs[0].coordinates.lon}}");
    //     let result = engine.find_template_dependencies(&template_value, &steps).unwrap();
    //     assert_eq!(result, vec![coordinates_id_clone.clone()]);

    //     // Test case 3: Template that references get_weather step (circular dependency scenario)
    //     let template_value = json!("{{steps.get_weather.outputs[0].weather.weather[0].description}}");
    //     let result = engine.find_template_dependencies(&template_value, &steps).unwrap();
    //     assert_eq!(result, vec![weather_id_clone.clone()]);

    //     // Test case 4: String without template dependencies
    //     let template_value = json!("regular string without templates");
    //     let result = engine.find_template_dependencies(&template_value, &steps).unwrap();
    //     assert_eq!(result, Vec::<String>::new());

    //     // Test case 5: Non-string value
    //     let template_value = json!(42);
    //     let result = engine.find_template_dependencies(&template_value, &steps).unwrap();
    //     assert_eq!(result, Vec::<String>::new());
    // }

    // TESTS

    #[tokio::test]
    async fn test_execute_action() {
        // Create a mock ExecutionEngine
        let engine = ExecutionEngine::new();
        
        // Test executing action with the same inputs as test_build_action_tree
        let action_ref = "starthubhq/get-weather-by-location-name:0.0.1";
        let inputs = vec![
            json!({
                "location_name": "Rome",
                "open_weather_api_key": "f13e712db9557544db878888528a5e29"
            })
        ];
        
        let result = engine.execute_action(action_ref, inputs).await;
        
        println!("result: {:#?}", result);
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
        
        // Verify execution order
        assert!(action_tree["execution_order"].is_array());
        let execution_order = action_tree["execution_order"].as_array().unwrap();
        assert_eq!(execution_order.len(), 2);
        assert_eq!(execution_order[0], "get_coordinates");
        assert_eq!(execution_order[1], "get_weather");
        
        // Verify types are present
        assert!(action_tree["types"].is_object());
        let types = action_tree["types"].as_object().unwrap();
        assert!(types.contains_key("WeatherConfig"));
        assert!(types.contains_key("CustomWeatherResponse"));
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
        
        // Verify execution order
        assert_eq!(action_tree.execution_order.len(), 2);
        assert_eq!(action_tree.execution_order[0], "get_coordinates");
        assert_eq!(action_tree.execution_order[1], "get_weather");
        
        // Verify types are present
        assert!(action_tree.types.is_some());
        let types = action_tree.types.as_ref().unwrap();
        assert!(types.contains_key("WeatherConfig"));
        assert!(types.contains_key("CustomWeatherResponse"));
    }

    

    #[tokio::test]
    async fn test_topological_sort_composition_steps() {
        // Create a mock ExecutionEngine
        let engine = ExecutionEngine::new();
        
        // Create mock steps with dependencies
        let mut steps = HashMap::new();
        
        // Step 2: get_weather - depends on get_coordinates
        let weather_id = uuid::Uuid::new_v4().to_string();
        steps.insert("get_weather".to_string(), ShAction {
            id: weather_id,
            name: "get_weather".to_string(),
            kind: "composition".to_string(),
            uses: "starthubhq/openweather-current-weather:0.0.1".to_string(),
            inputs: vec![
                ShIO {
                    name: "weather_config".to_string(),
                    r#type: "WeatherConfig".to_string(),
                    template: json!({
                        "lat": "{{steps.get_coordinates.outputs[0].coordinates.lat}}",
                        "lon": "{{steps.get_coordinates.outputs[0].coordinates.lon}}",
                        "open_weather_api_key": "{{inputs.weather_config.open_weather_api_key}}"
                    }),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            execution_order: vec![],
            types: None,
        });
        
        // Step 1: get_coordinates - no dependencies
        let coordinates_id = uuid::Uuid::new_v4().to_string();
        steps.insert("get_coordinates".to_string(), ShAction {
            id: coordinates_id,
            name: "get_coordinates".to_string(),
            kind: "composition".to_string(),
            uses: "starthubhq/openweather-coordinates-by-location-name:0.0.1".to_string(),
            inputs: vec![
                ShIO {
                    name: "open_weather_config".to_string(),
                    r#type: "OpenWeatherConfig".to_string(),
                    template: json!({
                        "location_name": "{{inputs[0].location_name}}",
                        "open_weather_api_key": "{{inputs[0].open_weather_api_key}}"
                    }),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            execution_order: vec![],
            types: None,
        });
        

        // Test the topological sort
        let sorted_steps = engine.topological_sort_composition_steps(&steps).await.unwrap();
        
        println!("Sorted steps: {:#?}", sorted_steps);
        // Just assert that sorted steps are an array ["get_coordinates", "get_weather"]
        assert_eq!(sorted_steps, vec!["get_coordinates", "get_weather"]);
        // Verify all steps are included
        assert_eq!(sorted_steps.len(), 2);
    }

    #[tokio::test]
    async fn test_topological_sort_single_step() {
        // Create a mock ExecutionEngine
        let engine = ExecutionEngine::new();
        
        // Create mock steps with just one step
        let mut steps = HashMap::new();
        
        // Single step: get_coordinates - no dependencies
        let coordinates_id = uuid::Uuid::new_v4().to_string();
        steps.insert("get_coordinates".to_string(), ShAction {
            id: coordinates_id,
            name: "get_coordinates".to_string(),
            kind: "composition".to_string(),
            uses: "starthubhq/openweather-coordinates-by-location-name:0.0.1".to_string(),
            inputs: vec![
                ShIO {
                    name: "open_weather_config".to_string(),
                    r#type: "OpenWeatherConfig".to_string(),
                    template: json!({
                        "location_name": "{{inputs[0].location_name}}",
                        "open_weather_api_key": "{{inputs[0].open_weather_api_key}}"
                    }),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            execution_order: vec![],
            types: None,
        });

        // Test the topological sort
        let sorted_steps = engine.topological_sort_composition_steps(&steps).await.unwrap();
        
        // Just assert that sorted steps contains only the single step
        assert_eq!(sorted_steps, vec!["get_coordinates"]);
        assert_eq!(sorted_steps.len(), 1);
    }

    #[tokio::test]
    async fn test_topological_sort_circular_dependency() {
        // Create a mock ExecutionEngine
        let engine = ExecutionEngine::new();
        
        // Create mock steps with circular dependency
        let mut steps = HashMap::new();
        
        // Step 1: depends on step 2
        steps.insert("step1".to_string(), ShAction {
            id: "step1".to_string(),
            name: "step1".to_string(),
            kind: "wasm".to_string(),
            uses: "test:action1".to_string(),
            inputs: vec![
                ShIO {
                    name: "input1".to_string(),
                    r#type: "string".to_string(),
                    template: json!("{{steps.step2.outputs[0].result}}"),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            execution_order: vec![],
            types: None,
        });
        
        // Step 2: depends on step 1 (circular dependency)
        steps.insert("step2".to_string(), ShAction {
            id: "step2".to_string(),
            name: "step2".to_string(),
            kind: "wasm".to_string(),
            uses: "test:action2".to_string(),
            inputs: vec![
                ShIO {
                    name: "input2".to_string(),
                    r#type: "string".to_string(),
                    template: json!("{{steps.step1.outputs[0].result}}"),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            execution_order: vec![],
            types: None,
        });

        // Test that circular dependency is detected
        let result = engine.topological_sort_composition_steps(&steps).await;
        assert!(result.is_err(), "Should detect circular dependency");
        
        if let Err(e) = result {
            assert!(e.to_string().contains("Circular dependency detected"));
        }
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
        let engine = ExecutionEngine::new();
        
        let mut wasm_action = ShAction {
            id: "test-wasm".to_string(),
            name: "test-wasm".to_string(),
            kind: "wasm".to_string(),
            uses: "test:wasm".to_string(),
            inputs: vec![],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            execution_order: vec![],
            types: None,
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
        
        let result = engine.instantiate(&types, &io_definitions, &io_values);
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
        
        let result = engine.instantiate(&types, &io_definitions, &io_values);
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
        
        let result = engine.instantiate(&types, &io_definitions, &io_values);
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
        
        let result = engine.instantiate(&types, &io_definitions, &io_values);
        assert!(result.is_ok(), "instantiate should succeed for object type");
        let instantiated = result.unwrap();
        assert_eq!(instantiated.len(), 1);
        assert_eq!(instantiated[0], json!({"key": "value", "nested": {"inner": 123}}));
    }

    #[test]
    fn test_instantiate_custom_types() {
        let engine = ExecutionEngine::new();
        
        // Create custom type definition
        let mut types = HashMap::new();
        types.insert("WeatherConfig".to_string(), json!({
            "location_name": {
                "type": "string",
                "description": "The name of the location",
                "required": true
            },
            "api_key": {
                "type": "string",
                "description": "API key for the service",
                "required": true
            },
            "temperature_unit": {
                "type": "string",
                "description": "Temperature unit (celsius/fahrenheit)",
                "required": false
            }
        }));
        
        let io_definitions = vec![
            ShIO {
                name: "weather_config".to_string(),
                r#type: "WeatherConfig".to_string(),
                template: json!({}),
                value: None,
                required: true,
            }
        ];
        
        // Test case 1: Valid custom type
        let valid_io_values = vec![json!({
            "location_name": "Rome",
            "api_key": "abc123",
            "temperature_unit": "celsius"
        })];
        
        let result = engine.instantiate(&Some(types.clone()), &io_definitions, &valid_io_values);
        assert!(result.is_ok(), "instantiate should succeed for valid custom type");
        let instantiated = result.unwrap();
        assert_eq!(instantiated.len(), 1);
        assert_eq!(instantiated[0], valid_io_values[0]);
        
        // Test case 2: Valid custom type with missing optional field
        let valid_io_values_minimal = vec![json!({
            "location_name": "Paris",
            "api_key": "def456"
        })];
        
        let result = engine.instantiate(&Some(types.clone()), &io_definitions, &valid_io_values_minimal);
        assert!(result.is_ok(), "instantiate should succeed for valid custom type with missing optional field");
        let instantiated = result.unwrap();
        assert_eq!(instantiated.len(), 1);
        assert_eq!(instantiated[0], valid_io_values_minimal[0]);
    }

    #[test]
    fn test_instantiate_validation_errors() {
        let engine = ExecutionEngine::new();
        
        // Create custom type definition
        let mut types = HashMap::new();
        types.insert("WeatherConfig".to_string(), json!({
            "location_name": {
                "type": "string",
                "description": "The name of the location",
                "required": true
            },
            "api_key": {
                "type": "string",
                "description": "API key for the service",
                "required": true
            }
        }));
        
        let io_definitions = vec![
            ShIO {
                name: "weather_config".to_string(),
                r#type: "WeatherConfig".to_string(),
                template: json!({}),
                value: None,
                required: true,
            }
        ];
        
        // Test case 1: Missing required field
        let invalid_io_values = vec![json!({
            "location_name": "Rome"
            // Missing required api_key field
        })];
        
        let result = engine.instantiate(&Some(types.clone()), &io_definitions, &invalid_io_values);
        assert!(result.is_err(), "instantiate should fail for missing required field");
        
        // Test case 2: Wrong field type
        let invalid_io_values_type = vec![json!({
            "location_name": 123, // Should be string, not number
            "api_key": "abc123"
        })];
        
        let result = engine.instantiate(&Some(types.clone()), &io_definitions, &invalid_io_values_type);
        assert!(result.is_err(), "instantiate should fail for wrong field type");
        
        // Test case 3: Extra fields not allowed (strict validation)
        let invalid_io_values_extra = vec![json!({
            "location_name": "Rome",
            "api_key": "abc123",
            "extra_field": "not_allowed" // This should cause validation to fail
        })];
        
        let result = engine.instantiate(&Some(types.clone()), &io_definitions, &invalid_io_values_extra);
        assert!(result.is_err(), "instantiate should fail for extra fields not in schema");
    }

    #[test]
    fn test_instantiate_no_types() {
        let engine = ExecutionEngine::new();
        
        let io_definitions = vec![
            ShIO {
                name: "test_string".to_string(),
                r#type: "string".to_string(), // Use primitive type
                template: json!(""),
                value: None,
                required: true,
            }
        ];
        let io_values = vec![json!("hello world")];
        let types = None; // No types provided
        
        let result = engine.instantiate(&types, &io_definitions, &io_values);
        assert!(result.is_ok(), "instantiate should succeed when no types are provided");
        let instantiated = result.unwrap();
        assert_eq!(instantiated.len(), 1);
        assert_eq!(instantiated[0], json!("hello world"));
    }

    #[test]
    fn test_instantiate_mixed_types() {
        let engine = ExecutionEngine::new();
        
        // Create custom type definition
        let mut types = HashMap::new();
        types.insert("WeatherConfig".to_string(), json!({
            "location_name": {
                "type": "string",
                "description": "The name of the location",
                "required": true
            },
            "api_key": {
                "type": "string",
                "description": "API key for the service",
                "required": true
            }
        }));
        
        let io_definitions = vec![
            ShIO {
                name: "location".to_string(),
                r#type: "string".to_string(), // Primitive type
                template: json!(""),
                value: None,
                required: true,
            },
            ShIO {
                name: "weather_config".to_string(),
                r#type: "WeatherConfig".to_string(), // Custom type
                template: json!({}),
                value: None,
                required: true,
            },
            ShIO {
                name: "temperature".to_string(),
                r#type: "number".to_string(), // Primitive type
                template: json!(0),
                value: None,
                required: true,
            }
        ];
        
        let io_values = vec![
            json!("Rome"), // For string type
            json!({ // For custom type
                "location_name": "Rome",
                "api_key": "abc123"
            }),
            json!(25.5) // For number type
        ];
        
        let result = engine.instantiate(&Some(types), &io_definitions, &io_values);
        assert!(result.is_ok(), "instantiate should succeed for mixed types");
        let instantiated = result.unwrap();
        assert_eq!(instantiated.len(), 3);
        assert_eq!(instantiated[0], json!("Rome"));
        assert_eq!(instantiated[1], json!({
            "location_name": "Rome",
            "api_key": "abc123"
        }));
        assert_eq!(instantiated[2], json!(25.5));
    }

    #[test]
    fn test_instantiate_empty_inputs() {
        let engine = ExecutionEngine::new();
        
        let io_definitions = vec![];
        let io_values = vec![];
        let types = None;
        
        let result = engine.instantiate(&types, &io_definitions, &io_values);
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
        
        let _result = engine.instantiate(&None, &io_definitions, &io_values);
        // This should panic due to unwrap() in the method when trying to access the second value
    }

    
    
}