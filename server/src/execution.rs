use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use dirs;
use reqwest::{self};
use petgraph::Graph;
use petgraph::algo::toposort;
use jsonschema::JSONSchema;

use crate::models::{ShManifest, ShKind, HubClient, ShIO, ShAction};

// Constants
const STARTHUB_API_BASE_URL: &str = "https://api.starthub.so";
const STARTHUB_STORAGE_PATH: &str = "/storage/v1/object/public/artifacts";
const STARTHUB_MANIFEST_FILENAME: &str = "starthub-lock.json";
pub struct ExecutionEngine {
    client: HubClient,
    cache_dir: std::path::PathBuf,
}

impl ExecutionEngine {
    pub fn new(base_url: String, token: Option<String>) -> Self {
        let cache_dir = dirs::cache_dir()
            .unwrap_or(std::env::temp_dir())
            .join("starthub/oci");
        
        // Ensure the cache directory exists
        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            eprintln!("Warning: Failed to create cache directory {:?}: {}", cache_dir, e);
        }
        
        Self {
            client: HubClient::new(base_url, token),
            cache_dir,
        }
    }

    pub async fn execute_action(&self, action_ref: &str, inputs: Vec<Value>) -> Result<Value> {
        // Ensure cache directory exists before starting execution.
        // It should already exist, but just in case.
        if let Err(e) = std::fs::create_dir_all(&self.cache_dir) {
            return Err(anyhow::anyhow!("Failed to create cache directory: {}", e));
        }
        
        // 1. Build the action tree
        let mut root_action = self.build_action_tree(
            action_ref,         // Action reference to download
            None,               // No parent action ID (root)
        ).await?;     

        println!("Root action: {:#?}", root_action);
        self.run_action_tree(&mut root_action, inputs).await?;

        // Return the action tree (no execution)
        Ok(serde_json::to_value(root_action)?)
    }

    async fn run_action_tree(&self,
        action_state: &mut ShAction,
        inputs: Vec<Value>) -> Result<()> {

        // Run type checking
        let resolved_action: ShAction = self.resolve_inputs(action_state, &inputs)?;

        // Run the action tree recursively again,
        // this time with the resolved inputs.

        // The base condition is for the kind to be "wasm" or "docker".
        // If it's a composition, then we keep iterating recursively.

        // After we return from the execution, we run type checking against the outputs,
        // just the same way we did for inputs.

        // Inject the outputs in the output variables

        // If it's a composition and all steps have been executed, then
        // the aggregate all the outputs back at the higher level

        return Ok(());
    }

    fn resolve_json_path(&self, path: &str, inputs: &Vec<Value>) -> Result<Value> {
        let parts: Vec<&str> = path.split('.').collect();
        
        if parts.is_empty() {
            return Err(anyhow::anyhow!("Empty JSON path"));
        }
        
        let root = parts[0];
        let mut current = match root {
            "inputs" => Value::Array(inputs.iter().map(|input| serde_json::to_value(input).unwrap()).collect()),
            _ => return Err(anyhow::anyhow!("Unknown root variable: {}", root))
        };
        
        for part in &parts[1..] {
            current = match current {
                Value::Array(arr) => {
                    if let Ok(index) = part.parse::<usize>() {
                        arr.get(index)
                            .ok_or_else(|| anyhow::anyhow!("Index {} out of bounds", index))?
                            .clone()
                    } else {
                        return Err(anyhow::anyhow!("Invalid array index: {}", part));
                    }
                }
                Value::Object(obj) => {
                    obj.get(*part)
                        .ok_or_else(|| anyhow::anyhow!("Property '{}' not found", part))?
                        .clone()
                }
                _ => return Err(anyhow::anyhow!("Cannot access property '{}' on non-object", part))
            };
        }
        
        Ok(current)
    }

    // Returns true or false, depending on whether the injected values are co
    fn resolve_inputs(&self, action_state: &ShAction, inputs: &Vec<Value>) -> Result<ShAction> {        
        let mut resolved_action = action_state.clone();

        // We extract the types from the action state
        let types = &action_state.types;
        let inputs = &action_state.inputs;
        let mut type_checked_inputs = Vec::new();

        // For every value, find its corresponding input by index
        for (index, value) in inputs.iter().enumerate() {
            if let Some(input) = inputs.get(index) {
                // Find the type definition in the types object
                if let Some(types_map) = types {
                    if let Some(type_definition) = types_map.get(&input.r#type) {                        
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
                        
                        // Validate the value against the schema
                        // if compiled_schema.validate(&value.value).is_ok() {
                        //     println!("Value {} is valid", index);
                        //     type_checked_inputs.push(ShIO {
                        //         name: input.name.clone(),
                        //         r#type: input.r#type.clone(),
                        //         required: input.required,
                        //         template: value.template.clone(),
                        //         value: None,
                        //     });
                        // } else {
                        //     let error_list: Vec<_> = compiled_schema.validate(value).unwrap_err().collect();
                        //     println!("Value {} is invalid: {:?}", index, error_list);
                        //     return Err(anyhow::anyhow!("Value {} is invalid: {:?}", index, error_list));
                        // }
                    } else {
                        println!("Type '{}' not found in types", input.r#type);
                        return Err(anyhow::anyhow!("Type '{}' not found in types", input.r#type));                       
                    }
                } else {
                    println!("No types defined");
                    return Err(anyhow::anyhow!("No types defined"));
                }
            }
        }

        resolved_action.inputs = type_checked_inputs;
        
        return Ok(resolved_action); // All values are valid
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
                schema.insert("type".to_string(), Value::String(s.clone()));
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
                arr.iter().filter_map(|output| {
                    if let Some(obj) = output.as_object() {
                        Some(ShIO {
                            name: obj.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            r#type: obj.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            required: obj.get("required").and_then(|v| v.as_bool()).unwrap_or(false),
                            template: obj.get("value").cloned().unwrap_or(serde_json::Value::Null),
                            value: None,
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
                                required: obj.get("required").and_then(|v| v.as_bool()).unwrap_or(false),
                                template: obj.get("value").cloned().unwrap_or(serde_json::Value::Null),
                                value: None,
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
        for (step_name, step_value) in manifest.steps {
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
                    action_state.steps.insert(child_action.id.clone(), child_action);
                }
            }
        }

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

    async fn topological_sort_composition_steps(&self, children: &HashMap<String, ShAction>) -> Result<Vec<String>> {
        // Build dependency graph using petgraph
        let mut graph = Graph::<String, ()>::new();
        let mut node_map = HashMap::new();
        
        // Add all child actions as nodes
        for (child_id, _) in children {
            let node = graph.add_node(child_id.clone());
            node_map.insert(child_id.clone(), node);
        }
        
        // Analyze dependencies by looking at template variables in child action inputs
        for (child_id, child_action) in children {
            let step_deps = self.find_template_dependencies(&serde_json::to_value(&child_action.inputs)?)?;
            for dep in step_deps {
                if let (Some(&current_node), Some(&dep_node)) = (node_map.get(child_id), node_map.get(&dep)) {
                    graph.add_edge(dep_node, current_node, ());
                }
            }
        }
        
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
    
    fn find_template_dependencies(&self, value: &Value) -> Result<Vec<String>> {
        match value {
            Value::String(s) => {
                // Look for patterns like {{step_name.field}} (but not {{inputs.field}})
                let re = regex::Regex::new(r"\{\{([^}]+)\}\}")?;
                let mut deps = Vec::new();
                
                for cap in re.captures_iter(s) {
                    if let Some(match_str) = cap.get(1) {
                        let template = match_str.as_str();
                        // Only extract step dependencies, not input dependencies
                        if !template.starts_with("inputs.") {
                            // Extract step name from template (e.g., "get_coordinates[0].coordinates.lat" -> "get_coordinates")
                            let step_name = if let Some(bracket_pos) = template.find('[') {
                                &template[..bracket_pos]
                            } else if let Some(dot_pos) = template.find('.') {
                                // Fallback for old format without brackets
                                &template[..dot_pos]
                            } else {
                                continue;
                            };
                            
                            let step_name = step_name.to_string();
                            if !deps.contains(&step_name) {
                                deps.push(step_name);
                            }
                        }
                    }
                }
                Ok(deps)
            },
            Value::Object(obj) => {
                let mut all_deps = Vec::new();
                for (_, v) in obj {
                    let child_deps = self.find_template_dependencies(v)?;
                    all_deps.extend(child_deps);
                }
                // Remove duplicates
                all_deps.sort();
                all_deps.dedup();
                Ok(all_deps)
            },
            Value::Array(arr) => {
                let mut all_deps = Vec::new();
                for item in arr {
                    let child_deps = self.find_template_dependencies(item)?;
                    all_deps.extend(child_deps);
                }
                // Remove duplicates
                all_deps.sort();
                all_deps.dedup();
                Ok(all_deps)
            },
            _ => Ok(Vec::new())
        }
    }

  }