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

        self.run_action_tree(&mut root_action, &inputs, &HashMap::new()).await?;

        // Return the action tree (no execution)
        Ok(serde_json::to_value(root_action)?)
    }

    async fn run_action_tree(&self,
        action_state: &mut ShAction,
        parent_inputs: &Vec<Value>,
        executed_sibling_steps: &HashMap<String, ShAction>) -> Result<()> {
        println!("_______________________________________________________");
        println!("action: {}", action_state.uses);
        println!("_______________________________________________________");

        // 1) Check whether the current action is ok with the inputs that are being passed to it.
        // Always resolve inputs first, independently of whether
        // we are dealing with a composition or an atomic action
        let instantiated_inputs: Vec<Value> = self.instantiate_inputs(
            &action_state.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()), 
            &action_state.inputs,
            &parent_inputs
        )?;

        println!("Instantiated inputs to be applied to the current action: {:#?}", instantiated_inputs);
        // Base condition
        // TODO: we need to figure out what to really return from the leaves
        if action_state.kind == "wasm" || action_state.kind == "docker" {
            println!("Executing atomic action: {}", action_state.uses);
            let inputs_value = serde_json::to_value(&instantiated_inputs)?;
            let result = self.run_wasm_step(action_state, None, &inputs_value).await?;
            
            println!("Result: {:#?}", result);
            
            return Ok(());
        }
        
        // For every input, want to assign the value of the corresponding resolved
        // input to its value field.
        for (index, input) in action_state.inputs.iter_mut().enumerate() {
            if let Some(resolved_input) = instantiated_inputs.get(index) {
                input.value = Some(resolved_input.clone());
            }
        }

        // Track executed steps as we go
        let mut local_executed_steps = executed_sibling_steps.clone();
        
        // Run the action tree recursively - DFS
        for step_id in &action_state.execution_order {
            
            if let Some(step) = action_state.steps.get_mut(step_id) {
                // For each step, we need to use the inputs and types field
                // of the step to generate a completely new object with that structure.
                
                // The inputs field determines not only the order of the inputs, but
                // also the structure of the input, along with how the inputs from the
                // current action or sibling need to be injected into each child step input.

                println!("Step id: {}", step_id);    
                // 2) Generate the inputs object that are going to be passed to the next recursion.
                // Resolve inputs for this step using the same logic as the main resolve_inputs function
                let child_step_resolved_inputs = self.resolve_template(
                    &step.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()),
                    &step.inputs,
                    &instantiated_inputs,  // Inject inputs from current action into the step
                    &local_executed_steps
                )?;

                println!("Child step resolved inputs to be passed to next recursion: {:#?}", child_step_resolved_inputs);

                // Clone the step before executing to avoid borrow checker issues
                let step_clone = step.clone();
                let step_name = step_clone.name.clone();
                
                // Execute the step with its own raw inputs, parent inputs for template resolution, and executed steps
                Box::pin(self.run_action_tree(
                    step, 
                    &child_step_resolved_inputs,  // Parent's resolved inputs for template resolution
                    &local_executed_steps
                )).await?;
                
                // Add the executed step to our tracking
                local_executed_steps.insert(step_name, step_clone);
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

    fn instantiate_inputs(&self, types: &Option<HashMap<String, Value>>, 
        input_definitions: &Vec<ShIO>,
        input_values: &Vec<Value>) -> Result<Vec<Value>> {
        println!("Instantiate inputs");
        println!("instantiate_inputs: {:#?}", input_definitions);
        println!("input_values: {:#?}", input_values);
        println!("types: {:#?}", types);
        let mut instantiated_inputs: Vec<Value> = Vec::new();
        for (index, input) in input_definitions.iter().enumerate() {
            // For each input definition, we want to fetch the corresponding input value by index
            // and instantiate the input with the value.
            let instantiated_input = input_values.get(index).unwrap().clone();

            // Handle primitive types
            if input.r#type.as_str() == "string" || input.r#type.as_str() == "bool" || input.r#type.as_str() == "number" {
                instantiated_inputs.push(instantiated_input);
                continue;
            }

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

                    println!("4");
                    // Compile the JSON schema
                    let compiled_schema = match JSONSchema::compile(&json_schema) {
                        Ok(schema) => schema,
                        Err(e) => {
                            println!("Failed to compile schema for type '{}': {}", input.r#type, e);
                            return Err(anyhow::anyhow!("Failed to compile schema for type '{}': {}", input.r#type, e));
                        }
                    };

                    println!("5");
                    println!("actual_value: {:#?}", input_values.get(index));
                    // Validate the resolved template against the schema
                    if compiled_schema.validate(&instantiated_input).is_ok() {
                        instantiated_inputs.push(instantiated_input);
                        println!("5.5");
                    } else {
                        let error_list: Vec<_> = compiled_schema.validate(&instantiated_input).unwrap_err().collect();
                        println!("Value {} is invalid: {:?}", index, error_list);
                        println!("6.5");
                        return Err(anyhow::anyhow!("Value {} is invalid: {:?}", index, error_list));
                    }
                }
            }
        }
        Ok(instantiated_inputs)
    }

    // Given a key value types object, a list of input definitions,
    // a list of input values and a list of executed sibling steps,
    // it returns a list of type-checked, resolved inputs.
    fn resolve_template(&self,
        types: &Option<HashMap<String, Value>>, 
        input_definitions: &Vec<ShIO>,
        input_values: &Vec<Value>,
        executed_sibling_steps: &HashMap<String, ShAction>) -> Result<Vec<Value>> {        
        // We extract the types from the action state
        let mut resolved_inputs: Vec<Value> = Vec::new();

        println!("types: {:#?}", types);
        println!("input_definitions: {:#?}", input_definitions);
        println!("input_values: {:#?}", input_values);
        println!("executed_sibling_steps: {:#?}", executed_sibling_steps);

        // For every value, find its corresponding input by index
        for (index, _input) in input_definitions.iter().enumerate() {
            if let Some(input) = input_definitions.get(index) {
                // First, resolve the template to get the actual input value
                let interpolated_template = self.interpolate(
                    &input.template, 
                    // We need parent inputs since the input
                    // might be coming from a parent
                    input_values, 
                    // We need sibling steps since the input
                    // might be coming from a sibling that 
                    // has already been executed
                    executed_sibling_steps
                )?;

                println!("resolved_template: {:#?}", interpolated_template);
                println!("1");
                // Handle primitive types that don't need custom type definitions
                if input.r#type == "string" || input.r#type == "bool" || input.r#type == "number" {
                    println!("Primitive type: {}", input.r#type);
                    // For primitive types, just push the resolved value without schema validation
                    resolved_inputs.push(interpolated_template);
                    continue;
                }

                println!("2");
                resolved_inputs.push(interpolated_template);
            }
        }

        println!("6");

        Ok(resolved_inputs)
    }

    // Since the variables might becoming from the parent or the siblings, this
    // function needs to know the parent inputs and the steps that have already been executed.
    fn interpolate(&self, 
        template: &Value, 
        parent_inputs: &Vec<Value>, 
        executed_steps: &HashMap<String, ShAction>
    ) -> Result<Value> {
        println!("interpolate: {:#?}", template);
        match template {
            Value::String(s) => {
                println!("resolve_template_string: {:#?}", s);
                let resolved = self.interpolate_string(s, parent_inputs, executed_steps)?;
                Ok(Value::String(resolved))
            },
            Value::Object(obj) => {
                println!("resolve_template_object: {:#?}", obj);
                // Recursively resolve object templates
                let mut resolved_obj = serde_json::Map::new();
                for (key, value) in obj {
                    let resolved_value = self.interpolate(value, parent_inputs, executed_steps)?;
                    resolved_obj.insert(key.clone(), resolved_value);
                }
                Ok(Value::Object(resolved_obj))
            },
            Value::Array(arr) => {
                println!("resolve_template_array: {:#?}", arr);
                // Recursively resolve array templates
                let mut resolved_arr = Vec::new();
                for item in arr {
                    let resolved_item = self.interpolate(item, parent_inputs, executed_steps)?;
                    resolved_arr.push(resolved_item);
                }
                Ok(Value::Array(resolved_arr))
            },
            _ => Ok(template.clone())
        }
    }

    fn interpolate_string(&self, 
        template: &str, 
        parent_inputs: &Vec<Value>, 
        executed_steps: &HashMap<String, ShAction>
    ) -> Result<String> {
        let mut result = template.to_string();
        
        // Handle {{inputs[index].field}} patterns
        let inputs_re = regex::Regex::new(r"\{\{inputs\[(\d+)\]\.([^}]+)\}\}")?;
        for cap in inputs_re.captures_iter(template) {
            if let (Some(index_str), Some(field)) = (cap.get(1), cap.get(2)) {
                if let Ok(index) = index_str.as_str().parse::<usize>() {
                    if let Some(input_value) = parent_inputs.get(index) {
                        if let Some(resolved_value) = input_value.get(field.as_str()) {
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
        
        // Handle {{steps.step_name.outputs[index].field}} patterns
        let steps_re = regex::Regex::new(r"\{\{steps\.([^.]+)\.outputs\[(\d+)\]\.([^}]+)\}\}")?;
        for cap in steps_re.captures_iter(template) {
            if let (Some(step_name), Some(index_str), Some(field)) = (cap.get(1), cap.get(2), cap.get(3)) {
                if let Ok(index) = index_str.as_str().parse::<usize>() {
                    if let Some(step) = executed_steps.get(step_name.as_str()) {
                        if let Some(output) = step.outputs.get(index) {
                            if let Some(output_value) = &output.value {
                                if let Some(resolved_value) = output_value.get(field.as_str()) {
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
    ) -> Result<Value> {
        if which::which("wasmtime").is_err() {
            bail!("wasmtime not found in PATH");
        }

        // For now, we'll create a simple implementation that downloads the WASM file
        // In a real implementation, this would download from the registry
        let module_path = self.download_wasm(&action.uses).await?;
        
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

        println!("Running WASM file: {:?}", module_path);
        println!("Input json: {:#?}", input_json);
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
            bail!("step '{}' failed with {}", action.id, status);
        }

        // Collect all results from the action
        let mut results = Vec::new();
        while let Ok(v) = rx.try_recv() { 
            results.push(v);
        }
        
        // Return the last result or an empty object if no results
        Ok(results.last().cloned().unwrap_or_else(|| serde_json::json!({})))
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
                    template: json!("{{inputs[0].location_name}}"),
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
        
        let result = engine.resolve_template(
            &action_state.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()), 
            &action_state.inputs,
            &valid_inputs,
            &HashMap::new());
        assert!(result.is_ok(), "resolve_inputs should succeed for valid inputs");
        
        let resolved_inputs = result.unwrap();
        assert_eq!(resolved_inputs.len(), 1);
        // Since resolve_template_string is not implemented yet, templates are returned as-is
        // The resolved value will be the template string, not the actual value
        assert_eq!(resolved_inputs[0], json!("{{inputs[0].location_name}}"));
        
        // Test case 2: Invalid inputs that don't match the schema (missing required field)
        let invalid_inputs = vec![
            json!({
                "location_name": "Rome"
                // Missing open_weather_api_key
            })
        ];
        
        let result = engine.resolve_template(&action_state.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()), 
        &action_state.inputs,
        &invalid_inputs,
        &HashMap::new());
        assert!(result.is_err(), "resolve_inputs should fail for invalid inputs");
        
        // Test case 3: No inputs provided
        let empty_inputs = vec![];
        let result = engine.resolve_template(&action_state.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()), 
        &action_state.inputs,
        &empty_inputs,
        &HashMap::new());
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
        
        let result = engine.resolve_template(&action_state_no_types.types.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>()), 
        &action_state_no_types.inputs,
        &valid_inputs,
        &HashMap::new());
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

    
    
}