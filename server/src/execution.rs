use anyhow::Result;
use jsonschema::JSONSchema;
use serde_json::Value;
use std::collections::HashMap;
use dirs;
use reqwest::{self};
use petgraph::Graph;
use petgraph::algo::toposort;

use crate::models::{ShManifest, ShKind, ShIO, ShAction};

// Constants
const STARTHUB_API_BASE_URL: &str = "https://api.starthub.so";
const STARTHUB_STORAGE_PATH: &str = "/storage/v1/object/public/artifacts";
const STARTHUB_MANIFEST_FILENAME: &str = "starthub-lock.json";
pub struct ExecutionEngine {
    cache_dir: std::path::PathBuf,
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

        // Print root action
        // println!("Root action: {:#?}", root_action);
        self.run_action_tree(&mut root_action, inputs).await?;

        // Return the action tree (no execution)
        Ok(serde_json::to_value(root_action)?)
    }

    async fn run_action_tree(&self,
        action_state: &mut ShAction,
        inputs: Vec<Value>) -> Result<()> {

        // Base condition
        // TODO: we need to figure out what to really return from the leaves
        if action_state.kind == "wasm" || action_state.kind == "docker" {
            return Ok(());
        }

        // Resolve inputs and run type checking
        let resolved_inputs: Vec<Value> = self.resolve_inputs(
            &action_state.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()), 
            &action_state.inputs,
            &inputs)?;
        
        println!("Resolved inputs: {:#?}", resolved_inputs);
        // For every input, want to assign the value of the corresponding index
        // in the resolved_inputs vector
        for (index, input) in action_state.inputs.iter_mut().enumerate() {
            if let Some(resolved_input) = resolved_inputs.get(index) {
                input.value = Some(resolved_input.clone());
            }
        }

        println!("Action state: {:#?}", action_state);
        println!("+++++++++++++++++++++++++++++++++++++++++++");
        // // Run the action tree recursively - DFS
        for step_id in &action_state.execution_order {
            if let Some(step) = action_state.steps.get_mut(step_id) {
                Box::pin(self.run_action_tree(step, resolved_inputs.clone())).await?;
            }
        }

        // The base condition is for the kind to be "wasm" or "docker".
        // If it's a composition, then we keep iterating recursively.

        // After we return from the execution, we run type checking against the outputs,
        // just the same way we did for inputs.

        // Inject the outputs in the output variables

        // If it's a composition and all steps have been executed, then
        // the aggregate all the outputs back at the higher level

        return Ok(());
    }

    // Returns true or false, depending on whether the injected values are co
    fn resolve_inputs(&self,
        types: &Option<HashMap<String, Value>>, 
        action_inputs: &Vec<ShIO>,
        inputs: &Vec<Value>) -> Result<Vec<Value>> {        
        // We extract the types from the action state
        let mut resolved_inputs: Vec<Value> = Vec::new();

        // For every value, find its corresponding input by index
        for (index, _input) in action_inputs.iter().enumerate() {
            if let Some(input) = action_inputs.get(index) {
                
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
                        if let Some(actual_value) = inputs.get(index) {
                            if compiled_schema.validate(actual_value).is_ok() {
                                println!("Value is valid {}: {}", index, actual_value);
                                resolved_inputs.push(actual_value.clone());
                            } else {
                                let error_list: Vec<_> = compiled_schema.validate(actual_value).unwrap_err().collect();
                                println!("Value {} is invalid: {:?}", index, error_list);
                                return Err(anyhow::anyhow!("Value {} is invalid: {:?}", index, error_list));
                            }
                        } else {
                            return Err(anyhow::anyhow!("No value provided for input {}", index));
                        }
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

        Ok(resolved_inputs)
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
                let step_deps = self.find_template_dependencies(&serde_json::to_value(&input.template)?, steps)?;
                
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
    
    pub fn find_template_dependencies(&self, value: &Value, steps: &HashMap<String, ShAction>) -> Result<Vec<String>> {        
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
                    let child_deps = self.find_template_dependencies(v, steps)?;
                    deps.extend(child_deps);
                }
                Ok(deps.into_iter().collect())
            },
            // If the value is an array, we need to find the dependencies in the array
            // recursively
            Value::Array(arr) => {
                for item in arr {
                    let child_deps = self.find_template_dependencies(item, steps)?;
                    deps.extend(child_deps);
                }
                Ok(deps.into_iter().collect())
            },
            _ => Ok(Vec::new())
        }
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

    #[test]
    fn test_resolve_inputs() {
        // Create a mock ExecutionEngine
        let engine = ExecutionEngine::new();
        
        // Create mock types for validation
        let mut types = HashMap::new();
        types.insert("WeatherConfig".to_string(), json!({
            "location_name": {
                "type": "string",
                "description": "The name of the location to get weather for",
                "required": true
            },
            "open_weather_api_key": {
                "type": "string", 
                "description": "OpenWeatherMap API key",
                "required": true
            }
        }));
        
        // Create mock action state with inputs and types
        let action_state = ShAction {
            id: "test-action".to_string(),
            name: "test-action".to_string(),
            kind: "composition".to_string(),
            uses: "test:action".to_string(),
            inputs: vec![
                ShIO {
                    name: "weather_config".to_string(),
                    r#type: "WeatherConfig".to_string(),
                    template: json!({}),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            execution_order: vec![],
            types: Some(types.clone().into_iter().collect()),
        };
        
        // Test case 1: Valid inputs that match the schema
        let valid_inputs = vec![
            json!({
                "location_name": "Rome",
                "open_weather_api_key": "f13e712db9557544db878888528a5e29"
            })
        ];
        
        let result = engine.resolve_inputs(
            &action_state.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()), 
            &action_state.inputs,
            &valid_inputs);
        assert!(result.is_ok(), "resolve_inputs should succeed for valid inputs");
        
        let resolved_inputs = result.unwrap();
        assert_eq!(resolved_inputs.len(), 1);
        assert_eq!(resolved_inputs[0]["location_name"], "Rome");
        assert_eq!(resolved_inputs[0]["open_weather_api_key"], "f13e712db9557544db878888528a5e29");
        
        // Test case 2: Invalid inputs that don't match the schema (missing required field)
        let invalid_inputs = vec![
            json!({
                "location_name": "Rome"
                // Missing open_weather_api_key
            })
        ];
        
        let result = engine.resolve_inputs(&action_state.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()), 
        &action_state.inputs,
        &invalid_inputs);
        assert!(result.is_err(), "resolve_inputs should fail for invalid inputs");
        
        // Test case 3: No inputs provided
        let empty_inputs = vec![];
        let result = engine.resolve_inputs(&action_state.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()), 
        &action_state.inputs,
        &empty_inputs);
        assert!(result.is_err(), "resolve_inputs should fail when no inputs provided");
        
        // Test case 4: Action state with no types defined
        let action_state_no_types = ShAction {
            id: "test-action".to_string(),
            name: "test-action".to_string(),
            kind: "composition".to_string(),
            uses: "test:action".to_string(),
            inputs: vec![
                ShIO {
                    name: "weather_config".to_string(),
                    r#type: "WeatherConfig".to_string(),
                    template: json!({}),
                    value: None,
                    required: true,
                }
            ],
            outputs: vec![],
            parent_action: None,
            steps: HashMap::new(),
            execution_order: vec![],
            types: None, // No types defined
        };
        
        let result = engine.resolve_inputs(&action_state_no_types.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()), 
        &action_state_no_types.inputs,
        &valid_inputs);
        assert!(result.is_err(), "resolve_inputs should fail when no types are defined");
    }

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
        let result = engine.run_action_tree(&mut wasm_action, inputs).await;
        
        // Should succeed and return early without processing
        assert!(result.is_ok(), "run_action_tree should succeed for wasm action");
    }

    
}