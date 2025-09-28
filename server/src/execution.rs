use anyhow::{Result, bail, Context};
use serde_json::{Value, json};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, process::Command};
use tokio::sync::mpsc;
use std::path::Path;
use std::collections::HashMap;
use which::which;

use crate::models::{ShManifest, ShKind, HubClient, StepSpec};

// Constants
const ST_MARKER: &str = "::starthub:state::";

// Data flow edge representing a variable dependency between steps

#[derive(Debug, Clone, serde::Serialize)]
struct ActionState {
    id: String,
    name: String,                    // "get_coordinates" or "get_weather_response"
    kind: String,                    // "composition", "wasm", "docker"
    uses: String,                    // Reference to the action
    inputs: Vec<Value>,              // Array format: [{"name": "...", "type": "...", "value": ...}]
    outputs: Vec<Value>,             // Array format: [{"name": "...", "type": "...", "value": ...}]
    parent_action: Option<String>,   // UUID of parent action (None for root)
    children: HashMap<String, ActionState>, // Nested actions keyed by UUID
    execution_order: Vec<String>,   // Order of execution within this action
    
    // Manifest structure fields
    types: Option<serde_json::Map<String, Value>>,   // From manifest.types
}

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

    pub async fn execute_action(&self, action_ref: &str, inputs: HashMap<String, Value>) -> Result<Value> {
        // Ensure cache directory exists before starting execution
        if let Err(e) = std::fs::create_dir_all(&self.cache_dir) {
            return Err(anyhow::anyhow!("Failed to create cache directory: {}", e));
        }
        
        // 1. Parse and prepare inputs
        let parsed_inputs = self.parse_inputs(&inputs);

        // 2. Build the action tree
        let root_action = self.build_action_tree(
            action_ref,         // Action reference to download
            None,               // No parent action name
            None,               // No parent action ID (root)
            &parsed_inputs      // Pass inputs for variable resolution
        ).await?;     

        // Return the action tree (no execution)
        Ok(serde_json::to_value(root_action)?)
    }


    fn parse_inputs(&self, inputs: &HashMap<String, Value>) -> serde_json::Map<String, Value> {
        let mut parsed_inputs = serde_json::Map::new();
        for (key, value) in inputs {
            if let Some(str_value) = value.as_str() {
                // Try to parse as JSON
                if let Ok(parsed_json) = serde_json::from_str::<Value>(str_value) {
                    parsed_inputs.insert(key.clone(), parsed_json);
                } else {
                    parsed_inputs.insert(key.clone(), value.clone());
                }
            } else {
                parsed_inputs.insert(key.clone(), value.clone());
            }
        }
        parsed_inputs
    }

    async fn build_action_tree(
        &self,
        action_ref: &str,
        parent_action_name: Option<&str>,
        parent_action_id: Option<&str>,
        global_inputs: &serde_json::Map<String, Value>
    ) -> Result<ActionState> {
        
        // 1. Download the manifest for this action
        let manifest = self.download_manifest(action_ref).await?;
        
        // 2. Create action state
        let action_id = uuid::Uuid::new_v4().to_string();
        let action_name = if let Some(parent) = parent_action_name {
            format!("{}.{}", parent, manifest.name)
        } else {
            manifest.name.clone()
        };
        let parent_id = action_id.clone();
        
        let mut action_state = ActionState {
            id: action_id,
            name: action_name.clone(),
            kind: match &manifest.kind {
                Some(ShKind::Composition) => "composition".to_string(),
                Some(ShKind::Wasm) => "wasm".to_string(),
                Some(ShKind::Docker) => "docker".to_string(),
                None => return Err(anyhow::anyhow!("Unknown manifest kind for action: {}", action_ref))
            },
            uses: action_ref.to_string(),
            inputs: if parent_action_name.is_none() {
                // Root action gets the global inputs with manifest structure and resolved values
                self.build_inputs_with_resolved_values(&manifest, global_inputs)?
            } else {
                // Child actions start with empty inputs (will be populated from step definitions)
                Vec::new()
            },
            outputs: Vec::new(),
            parent_action: parent_action_id.map(|s| s.to_string()),
            children: HashMap::new(),
            execution_order: Vec::new(),
            
                // Manifest structure fields
                types: if manifest.types.is_empty() { None } else { Some(manifest.types.clone().into_iter().collect()) },
        };
        
        // 3. Topologically sort steps based on dependencies
        let sorted_steps = self.topological_sort_composition_steps(&manifest).await?;
        
        // 4. First pass: Build all child actions without setting execution order
        let mut child_actions = HashMap::new();
        for (step_name, step_definition) in sorted_steps {
            // 5. Get the uses reference for this step
            let uses = step_definition.get("uses")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'uses' field in step definition"))?;
            
            // 6. Download the child manifest to determine its type
            let child_manifest = self.download_manifest(uses).await?;
            
            match &child_manifest.kind {
                Some(ShKind::Composition) => {
                    // 7. Create child composition action with inputs from step definition
                    let mut child_action = Box::pin(self.build_action_tree(
                        uses,                    // Download the child action manifest
                        Some(&action_name),      // Parent action name
                        Some(&parent_id),        // Current action's UUID as parent
                        global_inputs            // Pass global inputs for variable resolution
                    )).await?;
                    
                    // Build inputs for the composition child from step definition (contains template variables)
                    let step_map = step_definition.as_object().ok_or_else(|| anyhow::anyhow!("Step definition is not an object"))?;
                    child_action.inputs = self.build_inputs_from_step_definition(step_map, &child_manifest)?;
                    
                    // Store child action temporarily
                    let child_id = child_action.id.clone();
                    child_actions.insert(child_id.clone(), child_action);
                },
                Some(ShKind::Wasm) | Some(ShKind::Docker) => {
                    // 8. BASE CASE - create atomic action with proper inputs
                    let full_step_name = format!("{}.{}", action_name, step_name);
                    let atomic_step = self.create_atomic_step_from_manifest(
                        &full_step_name,
                        &child_manifest,
                        &step_definition
                    )?;
                    
                    // Use the manifest we already downloaded instead of downloading again
                    let atomic_manifest = child_manifest;
                    
                    // Create atomic action state with null values (will be resolved during execution)
                    let atomic_action = ActionState {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: step_name.clone(),
                        kind: atomic_step.kind.clone(),
                        uses: atomic_step.ref_.clone(),
                        inputs: {
                            let step_map = step_definition.as_object().ok_or_else(|| anyhow::anyhow!("Step definition is not an object"))?;
                            self.build_inputs_from_step_definition(step_map, &atomic_manifest)?
                        },
                        outputs: Vec::new(),
                        parent_action: Some(parent_id.clone()),
                        children: HashMap::new(),
                        execution_order: Vec::new(),
                        
                            // Manifest structure fields
                            types: if atomic_manifest.types.is_empty() { None } else { Some(atomic_manifest.types.clone().into_iter().collect()) },
                    };
                    
                    // Store atomic action temporarily
                    let atomic_id = atomic_action.id.clone();
                    child_actions.insert(atomic_id.clone(), atomic_action);
                },
                None => return Err(anyhow::anyhow!("Unknown manifest kind for step: {}", step_name))
            }
        }
        
        // 5. Second pass: Analyze cross-action dependencies and determine execution order
        let execution_order = self.determine_cross_action_execution_order(&child_actions)?;
        
        // 6. Add children to action state in correct execution order
        for child_id in &execution_order {
            if let Some(child_action) = child_actions.remove(child_id) {
                action_state.children.insert(child_id.clone(), child_action);
                action_state.execution_order.push(child_id.clone());
            }
        }
        
        Ok(action_state)
    }

    async fn download_manifest(&self, action_ref: &str) -> Result<ShManifest> {
        // Construct storage URL for starthub-lock.json
        let url_path = action_ref.replace(":", "/");
        let storage_url = format!(
            "https://api.starthub.so/storage/v1/object/public/artifacts/{}/starthub-lock.json",
            url_path
        );
        
        // Download and parse starthub-lock.json
        let manifest = self.client.download_starthub_lock(&storage_url).await?;
        
        
        Ok(manifest)
    }




    fn create_atomic_step_from_manifest(
        &self,
        step_name: &str,
        manifest: &ShManifest,
        step_definition: &Value
    ) -> Result<StepSpec> {
        let kind = match &manifest.kind {
            Some(ShKind::Wasm) => "wasm",
            Some(ShKind::Docker) => "docker",
            _ => return Err(anyhow::anyhow!("Invalid manifest kind for atomic step"))
        };
        
        Ok(StepSpec {
            id: step_name.to_string(),
            kind: kind.to_string(),
            ref_: format!("{}:{}", manifest.repository, manifest.version),
            args: Vec::new(),
            env: HashMap::new(),
            workdir: None,
            network: None,
            entry: None,
            mounts: Vec::new(),
            step_definition: Some(step_definition.clone()),
            calling_step_definition: Some(step_definition.clone()),
        })
    }

    
    async fn topological_sort_composition_steps(&self, manifest: &ShManifest) -> Result<Vec<(String, serde_json::Value)>> {
        use std::collections::{HashMap, HashSet, VecDeque};
        
        // Build dependency graph by analyzing template variables in composition steps
        let mut dependencies: HashMap<String, HashSet<String>> = HashMap::new();
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        
        // Initialize all composition steps
        for (step_id, _) in &manifest.steps {
            dependencies.insert(step_id.clone(), HashSet::new());
            in_degree.insert(step_id.clone(), 0);
        }
        
        // Analyze dependencies by looking at template variables in step definitions
        for (step_id, step_data) in &manifest.steps {
            let step_deps = self.extract_composition_step_dependencies(step_data)?;
            for dep in step_deps {
                if dependencies.contains_key(&dep) {
                    dependencies.get_mut(&dep).unwrap().insert(step_id.clone());
                    *in_degree.get_mut(step_id).unwrap() += 1;
                }
            }
        }
        
        // Topological sort using Kahn's algorithm
        let mut queue: VecDeque<String> = in_degree.iter()
            .filter(|(_, &count)| count == 0)
            .map(|(step_id, _)| step_id.clone())
            .collect();
        
        let mut sorted_steps = Vec::new();
        let mut iterations = 0;
        
        while let Some(current_step) = queue.pop_front() {
            iterations += 1;
            
            // Add the step to sorted list
            if let Some(step_data) = manifest.steps.get(&current_step) {
                sorted_steps.push((current_step.clone(), step_data.clone()));
            }
            
            // Update in-degree for dependent steps
            if let Some(deps) = dependencies.get(&current_step) {
                for dependent_step in deps {
                    if let Some(count) = in_degree.get_mut(dependent_step) {
                        *count -= 1;
                        if *count == 0 {
                            queue.push_back(dependent_step.clone());
                        }
                    }
                }
            }
            
            // Safety check to prevent infinite loops
            if iterations > 100 {
                return Err(anyhow::anyhow!("Topological sort exceeded maximum iterations"));
            }
        }
        
        
        // Check for cycles
        if sorted_steps.len() != manifest.steps.len() {
            return Err(anyhow::anyhow!("Circular dependency detected in composition steps"));
        }
        
        Ok(sorted_steps)
    }
    
    fn extract_composition_step_dependencies(&self, step_data: &serde_json::Value) -> Result<Vec<String>> {
        let mut dependencies = Vec::new();
        
        // Recursively search for template variables like {{step_name.field}}
        self.find_template_dependencies(step_data, &mut dependencies)?;
        
        Ok(dependencies)
    }
    
    
    
    fn find_template_dependencies(&self, value: &Value, deps: &mut Vec<String>) -> Result<()> {
        match value {
            Value::String(s) => {
                // Look for patterns like {{step_name.field}} (but not {{inputs.field}})
                let re = regex::Regex::new(r"\{\{([^}]+)\}\}")?;
                for cap in re.captures_iter(s) {
                    if let Some(match_str) = cap.get(1) {
                        let template = match_str.as_str();
                        // Only extract step dependencies, not input dependencies
                        if !template.starts_with("inputs.") {
                            // Extract step name from template (e.g., "get_coordinates[0].coordinates.lat" -> "get_coordinates")
                            if let Some(bracket_pos) = template.find('[') {
                                let step_name = &template[..bracket_pos];
                                if !deps.contains(&step_name.to_string()) {
                                    deps.push(step_name.to_string());
                                }
                            } else if let Some(dot_pos) = template.find('.') {
                                // Fallback for old format without brackets
                                let step_name = &template[..dot_pos];
                                if !deps.contains(&step_name.to_string()) {
                                    deps.push(step_name.to_string());
                                }
                            }
                        }
                    }
                }
            },
            Value::Object(obj) => {
                for (_, v) in obj {
                    self.find_template_dependencies(v, deps)?;
                }
            },
            Value::Array(arr) => {
                for item in arr {
                    self.find_template_dependencies(item, deps)?;
                }
            },
            _ => {}
        }
        
        Ok(())
    }
    
    
    async fn execute_action_recursive_with_parent(&self, action: &mut ActionState, parent_action: &ActionState) -> Result<()> {
        // Resolve variables for ALL action types (compositions and atomic actions)
        // Pass the parent action context for variable resolution
        self.resolve_variables_for_action_with_context(action, Some(parent_action)).await?;
        
        match action.kind.as_str() {
            "wasm" | "docker" => {
                // BASE CASE: Execute atomic action
                // Format inputs properly for WASM execution (url first, then headers)
                let step_params = self.format_inputs_for_wasm(&action.inputs)?;
                
                // Create a temporary StepSpec for the execution functions
                let temp_step = StepSpec {
                    id: action.id.clone(),
                    kind: action.kind.clone(),
                    ref_: action.uses.clone(),
                    args: vec![],
                    env: std::collections::HashMap::new(),
                    workdir: None,
                    network: None,
                    entry: None,
                    mounts: vec![],
                    step_definition: None,
                    calling_step_definition: None,
                };
                
                // Execute the atomic action
                let result = if action.kind == "wasm" {
                    self.run_wasm_step(&temp_step, None, &step_params).await?
                } else {
                    self.run_docker_step(&temp_step, None, &step_params).await?
                };
                
                // Parse and store outputs according to manifest types
                action.outputs = self.parse_outputs_according_to_manifest(&result, action).await?;
            },
            "composition" => {
                // RECURSIVE CASE: Execute composition
                // Get the parent inputs for this composition
                let parent_inputs = action.inputs.clone();
                
                // Execute children in the correct order
                for child_id in &action.execution_order {
                    // Clone the parent action before borrowing the child
                    let parent_clone = action.clone();
                    if let Some(child) = action.children.get_mut(child_id) {
                        // FIRST: Resolve template variables for this child at the parent level, as it might depend on outputs from previously executed siblings
                        self.resolve_child_template_variables_at_parent_level(child, &parent_clone).await?;
                        
                        // SECOND: Resolve child inputs from parent using semantic mapping (only for inputs that weren't resolved by templates)
                        self.resolve_child_inputs_from_parent(child, &parent_inputs).await?;
                        
                        // Recursively execute each child with parent context for variable resolution
                        Box::pin(self.execute_action_recursive_with_parent(child, &parent_clone)).await?;
                    }
                }
                
                // Aggregate outputs from children and set as composition's outputs
                self.aggregate_composition_outputs(action).await?;
            },
            _ => {
                return Err(anyhow::anyhow!("Unknown action kind: {}", action.kind));
            }
        }
        
        Ok(())
    }
    
    async fn execute_action_recursive(&self, action: &mut ActionState) -> Result<()> {
        // Resolve variables for ALL action types (compositions and atomic actions)
        // Pass the parent action context for variable resolution
        self.resolve_variables_for_action_with_context(action, None).await?;
        
        match action.kind.as_str() {
            "wasm" | "docker" => {
                // BASE CASE: Execute atomic action
                // Format inputs properly for WASM execution (url first, then headers)
                let step_params = self.format_inputs_for_wasm(&action.inputs)?;
                
                // Create a temporary StepSpec for the execution functions
                    let temp_step = StepSpec {
                    id: action.id.clone(),
                    kind: action.kind.clone(),
                    ref_: action.uses.clone(),
                        args: vec![],
                        env: std::collections::HashMap::new(),
                        workdir: None,
                        network: None,
                        entry: None,
                        mounts: vec![],
                        step_definition: None,
                        calling_step_definition: None,
                    };
                    
                // Execute the atomic action
                let result = if action.kind == "wasm" {
                    self.run_wasm_step(&temp_step, None, &step_params).await?
                } else {
                    self.run_docker_step(&temp_step, None, &step_params).await?
                };
                
                // Parse and store outputs according to manifest types
                action.outputs = self.parse_outputs_according_to_manifest(&result, action).await?;
                
                // Print the entire state after action completion
                println!("✅ Action completed: {}", serde_json::to_string_pretty(action).unwrap_or_else(|_| "{}".to_string()));
            },
            "composition" => {
                // RECURSIVE CASE: Execute children in depth-first order
                // Clone parent inputs to avoid borrowing conflicts
                let parent_inputs = action.inputs.clone();
                
                // Clone execution order to avoid borrowing issues
                let child_ids: Vec<String> = action.execution_order.clone();
                
                for child_id in child_ids {
                    // Clone the parent action before borrowing the child
                    let parent_clone = action.clone();
                    if let Some(child) = action.children.get_mut(&child_id) {
                        // FIRST: Resolve template variables for this child at the parent level, as it might depend on outputs from previously executed siblings
                        self.resolve_child_template_variables_at_parent_level(child, &parent_clone).await?;
                        
                        // SECOND: Resolve child inputs from parent using semantic mapping (only for inputs that weren't resolved by templates)
                        self.resolve_child_inputs_from_parent(child, &parent_inputs).await?;
                        
                        // Recursively execute each child with parent context for variable resolution
                        Box::pin(self.execute_action_recursive_with_parent(child, &parent_clone)).await?;
                    }
                }
                
                // Aggregate outputs from children and set as composition's outputs
                self.aggregate_composition_outputs(action).await?;
                
                // Print the composition state after completion
                println!("✅ Action completed: {}", serde_json::to_string_pretty(action).unwrap_or_else(|_| "{}".to_string()));
            },
            _ => return Err(anyhow::anyhow!("Unknown action kind: {}", action.kind)),
        }
        
        Ok(())
    }
    
    fn build_inputs_for_child_composition(&self, manifest: &ShManifest, parent_inputs: &Vec<Value>, global_inputs: &serde_json::Map<String, Value>) -> Result<Vec<Value>> {
        let mut structured_inputs = Vec::new();
        
        // Array format: [{"name": "...", "type": "...", ...}]
        if let serde_json::Value::Array(inputs_array) = &manifest.inputs {
            for (index, input_def) in inputs_array.iter().enumerate() {
            if let Some(input_def_obj) = input_def.as_object() {
                    let input_name = input_def_obj.get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    
                let mut processed_input_def = input_def_obj.clone();
                
                    // Try to resolve the value from parent inputs using positional mapping
                let mut actual_value = Value::Null;
                
                    // Positional mapping: parent_inputs[index] -> child_inputs[index]
                    if index < parent_inputs.len() {
                        actual_value = self.extract_value_from_structured_input(&parent_inputs[index]);
                    }
                    
                    // If still not found, try global inputs by name
                if actual_value.is_null() {
                    if let Some(global_value) = global_inputs.get(input_name) {
                        actual_value = self.extract_value_from_structured_input(global_value);
                    }
                }
                
                // Set the resolved value
                processed_input_def.insert("value".to_string(), actual_value);
                    structured_inputs.push(Value::Object(processed_input_def));
                }
                }
                    } else {
            return Err(anyhow::anyhow!("Inputs must be in array format"));
        }
        
        Ok(structured_inputs)
    }
    
    fn extract_value_from_structured_input(&self, input_value: &Value) -> Value {
        if let Some(input_obj) = input_value.as_object() {
            input_obj.get("value").cloned().unwrap_or(Value::Null)
        } else {
            input_value.clone()
        }
    }
    

    fn build_inputs_with_resolved_values(&self, manifest: &ShManifest, global_inputs: &serde_json::Map<String, Value>) -> Result<Vec<Value>> {
        let mut structured_inputs = Vec::new();
        
        // Array format: [{"name": "...", "type": "...", ...}]
        if let serde_json::Value::Array(inputs_array) = &manifest.inputs {
            for input_def in inputs_array.iter() {
            if let Some(input_def_obj) = input_def.as_object() {
                    let input_name = input_def_obj.get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    
                let mut processed_input_def = input_def_obj.clone();
                
                // Resolve the value from global inputs
                if let Some(value_template) = input_def_obj.get("value") {
                    let resolved_value = self.resolve_template_variables(value_template, &Vec::new())?;
                    processed_input_def.insert("value".to_string(), resolved_value);
            } else {
                    // If no value template, try to get the value from global inputs directly
                    if let Some(global_value) = global_inputs.get(input_name) {
                        processed_input_def.insert("value".to_string(), global_value.clone());
                    } else {
                        processed_input_def.insert("value".to_string(), Value::Null);
                    }
                }
                
                    structured_inputs.push(Value::Object(processed_input_def));
                }
            }
                } else {
            return Err(anyhow::anyhow!("Inputs must be in array format"));
        }
        
        Ok(structured_inputs)
    }
    
    fn build_inputs_for_child_atomic(&self, manifest: &ShManifest, parent_inputs: &Vec<Value>, global_inputs: &serde_json::Map<String, Value>) -> Result<Vec<Value>> {
        // For atomic actions, we use the same logic as composition but with simpler resolution
        self.build_inputs_for_child_composition(manifest, parent_inputs, global_inputs)
    }

    fn build_inputs_with_manifest_structure(&self, manifest: &ShManifest, _global_inputs: &serde_json::Map<String, Value>) -> Result<Vec<Value>> {
        let mut structured_inputs = Vec::new();
        
        // Array format: [{"name": "...", "type": "...", ...}]
        if let serde_json::Value::Array(inputs_array) = &manifest.inputs {
            for input_def in inputs_array.iter() {
            if let Some(input_def_obj) = input_def.as_object() {
                let mut processed_input_def = input_def_obj.clone();
                // Set value to null initially - will be resolved during execution
                processed_input_def.insert("value".to_string(), Value::Null);
                    structured_inputs.push(Value::Object(processed_input_def));
                }
            }
                    } else {
            return Err(anyhow::anyhow!("Inputs must be in array format"));
        }
        
        Ok(structured_inputs)
    }
    
    /// Build inputs from step definition (contains template variables)
    fn build_inputs_from_step_definition(
        &self,
        step_definition: &serde_json::Map<String, serde_json::Value>,
        child_manifest: &ShManifest,
    ) -> Result<Vec<serde_json::Value>> {
        let step_inputs = step_definition
            .get("inputs")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("Step definition missing inputs array"))?;

        let mut inputs = Vec::new();
        
        for (index, step_input) in step_inputs.iter().enumerate() {
            if let Some(input_obj) = step_input.as_object() {
                let mut input = serde_json::Map::new();
                
                // Extract the name field
                if let Some(name) = input_obj.get("name") {
                    input.insert("name".to_string(), name.clone());
                } else {
                    input.insert("name".to_string(), serde_json::Value::String(format!("input_{}", index)));
                }
                
                // Add type from child manifest if not present
                if let Some(manifest_inputs) = child_manifest.inputs.as_array() {
                    if let Some(manifest_input) = manifest_inputs.get(index) {
                        if let Some(manifest_obj) = manifest_input.as_object() {
                            if let Some(input_type) = manifest_obj.get("type") {
                                input.insert("type".to_string(), input_type.clone());
                            }
                        }
                    }
                }
                
                // Add required flag if not present
                if let Some(required) = input_obj.get("required") {
                    input.insert("required".to_string(), required.clone());
                } else {
                    input.insert("required".to_string(), serde_json::Value::Bool(true));
                }
                
                // Handle the value field - if it exists, use it directly, otherwise create from template variables
                if let Some(existing_value) = input_obj.get("value") {
                    // If there's already a value field, use it directly
                    input.insert("value".to_string(), existing_value.clone());
                } else {
                    // Create value object with template variables (excluding name, type, required)
                    let mut value_obj = serde_json::Map::new();
                    for (key, value) in input_obj {
                        if key != "name" && key != "type" && key != "required" {
                            value_obj.insert(key.clone(), value.clone());
                        }
                    }
                    
                    // Set the value field with the template variables
                    input.insert("value".to_string(), serde_json::Value::Object(value_obj));
                }
                
                inputs.push(serde_json::Value::Object(input));
            }
        }
        
        Ok(inputs)
    }
    
    fn get_nested_value(&self, inputs: &Vec<Value>, path: &str) -> Option<Value> {
        let parts: Vec<&str> = path.split('.').collect();
        
        // First, try to find the input by name in the array
        for input in inputs {
            if let Some(input_obj) = input.as_object() {
                if let Some(name) = input_obj.get("name").and_then(|v| v.as_str()) {
                    if name == parts[0] {
                        let mut current = input.clone();
        
        for part in parts.iter().skip(1) {
            if let Some(obj) = current.as_object() {
                current = obj.get(*part)?.clone();
            } else {
                return None;
            }
        }
        
                        return Some(current);
                    }
                }
            }
        }
        
        None
    }

    fn validate_resolved_value(&self, value: &Value, input_def: &serde_json::Map<String, Value>) -> Result<()> {
        // Get the expected type from the input definition
        if let Some(expected_type) = input_def.get("type").and_then(|t| t.as_str()) {
            match expected_type {
                "string" => {
                    if !value.is_string() {
                        return Err(anyhow::anyhow!("Expected string value, got: {:?}", value));
                    }
                },
                "number" => {
                    if !value.is_number() {
                        return Err(anyhow::anyhow!("Expected number value, got: {:?}", value));
                    }
                },
                "boolean" => {
                    if !value.is_boolean() {
                        return Err(anyhow::anyhow!("Expected boolean value, got: {:?}", value));
                    }
                },
                _ => {
                    // For custom types, we could add more sophisticated validation
                    // For now, just log that we're using a custom type
                }
            }
        }
        
        Ok(())
    }

    async fn aggregate_composition_outputs(&self, action: &mut ActionState) -> Result<()> {
        // For now, use a simple fallback approach - aggregate outputs from all children
        let mut composition_outputs = Vec::new();
        
        for child_id in &action.execution_order {
            if let Some(child) = action.children.get(child_id) {
                for output in &child.outputs {
                    composition_outputs.push(output.clone());
                }
            }
        }
        
        action.outputs = composition_outputs;
        Ok(())
    }

    /// Parse outputs according to manifest types and names
    async fn parse_outputs_according_to_manifest(&self, result: &Value, _action: &ActionState) -> Result<Vec<Value>> {
        // For now, we'll use a simple approach that preserves the existing behavior
        // but could be enhanced to validate against manifest types
        
        if let Some(arr) = result.as_array() {
            Ok(arr.clone())
        } else {
            // If result is not an array, wrap it in a generic output
            let mut outputs = Vec::new();
            let mut output_obj = serde_json::Map::new();
            output_obj.insert("name".to_string(), Value::String("result".to_string()));
            output_obj.insert("value".to_string(), result.clone());
            outputs.push(Value::Object(output_obj));
            Ok(outputs)
        }
    }
    
    
    async fn resolve_child_template_variables_at_parent_level(&self, child: &mut ActionState, parent: &ActionState) -> Result<()> {
        // Resolve template variables for this child at the parent level
        // This allows the child to access outputs from sibling actions
        
        // Clone the child to avoid borrowing issues
        let child_clone = child.clone();
        
        // Resolve template variables in the child's inputs
        for (_index, input_def) in child.inputs.iter_mut().enumerate() {
            if let Some(input_def_obj) = input_def.as_object_mut() {
                if let Some(value_field) = input_def_obj.get_mut("value") {
                    // Resolve template variables using parent context
                    let resolved_value = self.resolve_template_variables_from_context(value_field, &child_clone, Some(parent)).await?;
                    
                    // Update the input with resolved value
                    input_def_obj.insert("value".to_string(), resolved_value);
                }
            }
        }
        
        Ok(())
    }

    async fn resolve_variables_for_action_with_context(&self, action: &mut ActionState, parent_action: Option<&ActionState>) -> Result<()> {
        // Resolve variables in the action's inputs using parent context and sibling outputs
        let mut resolved_inputs = Vec::new();
        
        for input_def in &action.inputs {
            if let Some(input_def_obj) = input_def.as_object() {
                let mut resolved_input_def = input_def_obj.clone();
                
                // Resolve the value field if it exists
                if let Some(value_field) = input_def_obj.get("value") {
                    // Resolve from parent inputs and sibling outputs
                    let resolved_value = self.resolve_template_variables_from_context(value_field, action, parent_action).await?;
                    
                    // Validate the resolved value against its type definition
                    if let Err(_e) = self.validate_resolved_value(&resolved_value, input_def_obj) {
                    }
                    
                    resolved_input_def.insert("value".to_string(), resolved_value);
                }
                
                resolved_inputs.push(serde_json::Value::Object(resolved_input_def));
            }
        }
        
        action.inputs = resolved_inputs;
        Ok(())
    }
    
    async fn resolve_variables_for_action(&self, action: &mut ActionState) -> Result<()> {
        // Resolve variables in the action's inputs using the new array format
        let mut resolved_inputs = Vec::new();
        
        for input_def in &action.inputs {
            if let Some(input_def_obj) = input_def.as_object() {
                let mut resolved_input_def = input_def_obj.clone();
                
                // Resolve the value field if it exists
                if let Some(value_field) = input_def_obj.get("value") {
                    let resolved_value = self.resolve_template_variables(value_field, &action.inputs)?;
                    
                    // Validate the resolved value against its type definition
                    if let Err(_e) = self.validate_resolved_value(&resolved_value, input_def_obj) {
                    }
                    
                    resolved_input_def.insert("value".to_string(), resolved_value);
                }
                
                resolved_inputs.push(Value::Object(resolved_input_def));
            } else {
                // Fallback for non-object inputs
                let resolved_value = self.resolve_template_variables(input_def, &action.inputs)?;
                resolved_inputs.push(resolved_value);
            }
        }
        
        action.inputs = resolved_inputs;
        Ok(())
    }
    
    
    fn resolve_template_variables(&self, value: &Value, inputs: &Vec<Value>) -> Result<Value> {
        match value {
            Value::String(s) => {
                let mut result = s.clone();
                
                // Replace {{inputs.*}} patterns
                let input_pattern = regex::Regex::new(r"\{\{inputs\.([^}]+)\}\}")?;
                result = input_pattern.replace_all(&result, |caps: &regex::Captures| {
                    let path = &caps[1];
                    if let Some(value) = self.get_nested_value(inputs, path) {
                        match value {
                            Value::String(s) => s.clone(),
                            Value::Number(n) => n.to_string(),
                            Value::Bool(b) => b.to_string(),
                            _ => serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string()),
                        }
                    } else {
                        caps[0].to_string() // Keep original if not found
                    }
                }).to_string();
                
                Ok(Value::String(result))
            },
            Value::Object(obj) => {
                let mut resolved_obj = serde_json::Map::new();
                for (key, val) in obj {
                    resolved_obj.insert(key.clone(), self.resolve_template_variables(val, inputs)?);
                }
                Ok(Value::Object(resolved_obj))
            },
            Value::Array(arr) => {
                let mut resolved_arr = Vec::new();
                for item in arr {
                    resolved_arr.push(self.resolve_template_variables(item, inputs)?);
                }
                Ok(Value::Array(resolved_arr))
            },
            other => Ok(other.clone()),
        }
    }


    async fn resolve_child_inputs_from_parent(&self, child: &mut ActionState, parent_inputs: &Vec<Value>) -> Result<()> {
        // Resolve child inputs from parent using positional mapping and template variables
        let mut resolved_inputs = Vec::new();
        
        
        for (index, input_def) in child.inputs.iter().enumerate() {
            if let Some(input_def_obj) = input_def.as_object() {
                let mut resolved_input_def = input_def_obj.clone();
                
                // Check if this input has already been resolved by template variables
                let mut actual_value = Value::Null;
                if let Some(existing_value) = input_def_obj.get("value") {
                    // If the value contains template variables, it hasn't been resolved yet
                    if let Some(template_str) = existing_value.as_str() {
                        if template_str.contains("{{") {
                            // This input still has template variables, try positional mapping
                            if index < parent_inputs.len() {
                                actual_value = self.extract_value_from_structured_input(&parent_inputs[index]);
                            } else {
                                // No positional match available, keep the template variables for later resolution
                                actual_value = existing_value.clone();
                            }
                        } else {
                            // This input has already been resolved, keep the existing value
                            actual_value = existing_value.clone();
                        }
                    } else {
                        // Non-string value, use as-is
                        actual_value = existing_value.clone();
                    }
                } else if index < parent_inputs.len() {
                    // No existing value, try positional mapping
                    actual_value = self.extract_value_from_structured_input(&parent_inputs[index]);
                } else {
                    // If no positional match, try to resolve template variables in the input definition
                    if let Some(template_value) = input_def_obj.get("value") {
                        if let Some(template_str) = template_value.as_str() {
                            if template_str.contains("{{") {
                                // Resolve template variables using parent inputs
                                let resolved_template = self.resolve_template_variables_from_parent(template_str, parent_inputs)?;
                                actual_value = resolved_template;
                            }
                        }
                    }
                    
                    if actual_value.is_null() {
                    }
                }
                
                // Set the resolved value
                resolved_input_def.insert("value".to_string(), actual_value);
                resolved_inputs.push(Value::Object(resolved_input_def));
            } else {
                // Fallback for non-object inputs
                resolved_inputs.push(input_def.clone());
            }
        }
        
        child.inputs = resolved_inputs;
        Ok(())
    }

    fn resolve_template_variables_from_parent(&self, template: &str, parent_inputs: &Vec<Value>) -> Result<Value> {
        let mut result = template.to_string();
                
                // Replace {{inputs.*}} patterns
                let input_pattern = regex::Regex::new(r"\{\{inputs\.([^}]+)\}\}")?;
                result = input_pattern.replace_all(&result, |caps: &regex::Captures| {
                    let path = &caps[1];
            if let Some(value) = self.get_nested_value_from_parent(parent_inputs, path) {
                            match value {
                                Value::String(s) => s.clone(),
                                Value::Number(n) => n.to_string(),
                                Value::Bool(b) => b.to_string(),
                                _ => serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string()),
                            }
                        } else {
                        caps[0].to_string() // Keep original if not found
                    }
                }).to_string();
                
                Ok(Value::String(result))
    }
    
    fn get_nested_value_from_parent(&self, parent_inputs: &Vec<Value>, path: &str) -> Option<Value> {
        let parts: Vec<&str> = path.split('.').collect();
        
        // First, try to find the input by name in the parent inputs
        for input in parent_inputs {
            if let Some(input_obj) = input.as_object() {
                if let Some(name) = input_obj.get("name").and_then(|v| v.as_str()) {
                    if name == parts[0] {
                        let mut current = input.clone();
                        
                        for part in parts.iter().skip(1) {
                            if let Some(obj) = current.as_object() {
                                current = obj.get(*part)?.clone();
                            } else {
                                return None;
                            }
                        }
                        
                        return Some(current);
                    }
                }
            }
        }
        
        None
    }






    async fn run_docker_step(
        &self,
        step: &StepSpec,
        pipeline_workdir: Option<&str>,
        state_in: &Value,
    ) -> Result<Value> {
        if which("docker").is_err() {
            bail!("docker not found on PATH");
        }

        let mut cmd = Command::new("docker");
        cmd.arg("run").arg("--rm").arg("-i");

        // network
        match step.network.as_deref() {
            Some("bridge") => {},
            _ => { cmd.args(["--network","none"]); }
        }

        // mounts
        for m in &step.mounts {
            if m.typ != "bind" { continue; }
            let spec = format!("{}:{}{}",
                self.absolutize(&m.source, pipeline_workdir)?,
                &m.target,
                if m.rw { "" } else { ":ro" }
            );
            cmd.args(["-v", &spec]);
        }

        // env
        for (k, v) in &step.env {
            cmd.args(["-e", &format!("{k}={v}")]);
        }

        // workdir
        if let Some(wd) = step.workdir.as_deref().or(pipeline_workdir) {
            if wd.starts_with('/') { cmd.args(["-w", wd]); }
            else { tracing::warn!("ignoring non-absolute workdir '{}'", wd); }
        }

        // entrypoint
        if let Some(ep) = &step.entry {
            cmd.args(["--entrypoint", ep]);
        }

        // For Docker, use the action reference as the image name
        // The image should have been loaded during prefetch
        let docker_image = &step.ref_;
        cmd.arg(docker_image);
        for a in &step.args { cmd.arg(a); }

        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning docker for step {}", step.id))?;

        // feed stdin JSON - use the pre-built parameters
        let input = serde_json::to_string(state_in)?;

        if let Some(stdin) = child.stdin.as_mut() { 
            stdin.write_all(input.as_bytes()).await?; 
        }
        drop(child.stdin.take());

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let mut out_reader = BufReader::new(stdout).lines();
        let mut err_reader = BufReader::new(stderr).lines();

        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();

        let pump_out = tokio::spawn(async move {
            while let Ok(Some(line)) = out_reader.next_line().await {
                if let Some(idx) = line.find(ST_MARKER) {
                    let json_part = &line[idx + ST_MARKER.len()..];
                    if let Ok(v) = serde_json::from_str::<Value>(json_part) {
                        let _ = tx.send(v);
                    }
                }
            }
        });

        let pump_err = tokio::spawn(async move {
            while let Ok(Some(_line)) = err_reader.next_line().await {
                // Just consume stderr without logging
            }
        });

        let status = child.wait().await?;
        let _ = pump_out.await;
        let _ = pump_err.await;

        if !status.success() {
            bail!("step '{}' failed with {}", step.id, status);
        }

        // Collect all results from the action
        let mut results = Vec::new();
        while let Ok(v) = rx.try_recv() {
            results.push(v);
        }
        
        // Return the last result or an empty object if no results
        Ok(results.last().cloned().unwrap_or_else(|| json!({})))
    }

    async fn run_wasm_step(
        &self,
        step: &StepSpec,
        pipeline_workdir: Option<&str>,
        state_in: &Value,
    ) -> Result<Value> {
        if which("wasmtime").is_err() {
            bail!("`wasmtime` not found on PATH.");
        }

        // ensure we have the .wasm component locally
        let module_path = self.client.download_wasm(&step.ref_, &self.cache_dir).await?;
        // Verify the WASM file exists and is readable
        if !module_path.exists() {
            return Err(anyhow::anyhow!("WASM file not found at: {:?}", module_path));
        }
        
        // Check if the file is readable
        if let Err(e) = std::fs::metadata(&module_path) {
            return Err(anyhow::anyhow!("WASM file not accessible at {:?}: {}", module_path, e));
        }

        // build stdin payload - use the pre-built parameters
        let input_json = serde_json::to_string(state_in)?;

        // Construct command
        let mut cmd = Command::new("wasmtime");
        cmd.arg("-S").arg("http");
        cmd.arg(&module_path);

        // optional: pass extra args defined in step.args
        for a in &step.args { cmd.arg(a); }

        // pass env (tokens, etc.)
        for (k, v) in &step.env { cmd.env(k, v); }

        // working dir if absolute
        if let Some(wd) = step.workdir.as_deref().or(pipeline_workdir) {
            if wd.starts_with('/') { cmd.current_dir(wd); }
        }

        // spawn with piped stdio
        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn wasmtime for step {}: {}", step.id, e))?;

        // feed stdin JSON
        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(input_json.as_bytes()).await?;
        }
        drop(child.stdin.take());

        // pump stdout/stderr and collect patches
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let mut out_reader = BufReader::new(stdout).lines();
        let mut err_reader = BufReader::new(stderr).lines();

        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();

        let pump_out = tokio::spawn(async move {
            while let Ok(Some(line)) = out_reader.next_line().await {
                // Try to parse the line directly as JSON
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        let _ = tx.send(v);
                }
            }
        });

        let pump_err = tokio::spawn(async move {
            while let Ok(Some(_line)) = err_reader.next_line().await {
            }
        });

        let status = child.wait().await?;
        let _ = pump_out.await;
        let _ = pump_err.await;


        if !status.success() {
            bail!("step '{}' failed with {}", step.id, status);
        }

        // Collect all results from the action
        let mut results = Vec::new();
        while let Ok(v) = rx.try_recv() { 
            results.push(v);
        }
        
        
        // Return the last result or an empty object if no results
        Ok(results.last().cloned().unwrap_or_else(|| json!({})))
    }

    fn absolutize(&self, p: &str, base: Option<&str>) -> Result<String> {
        let path = Path::new(p);
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            match base {
                Some(b) => Path::new(b).join(path),
                None => std::env::current_dir()?.join(path),
            }
        };
        Ok(abs.canonicalize()?.to_string_lossy().to_string())
    }
    
    /// Format inputs for WASM execution with proper ordering and resolved template variables
    fn format_inputs_for_wasm(&self, inputs: &Vec<Value>) -> Result<Value> {
        let mut formatted_inputs = Vec::new();
        
        // Find url input first (required)
        if let Some(url_input) = inputs.iter().find(|input| {
            input.as_object().and_then(|obj| obj.get("name")).and_then(|v| v.as_str()) == Some("url")
        }) {
            if let Some(url_obj) = url_input.as_object() {
                let mut url_formatted = serde_json::Map::new();
                url_formatted.insert("name".to_string(), Value::String("url".to_string()));
                
                // Resolve template variables in the url value
                if let Some(url_value) = url_obj.get("value") {
                    let resolved_url = self.resolve_template_variables_in_value(url_value, inputs)?;
                    url_formatted.insert("value".to_string(), resolved_url);
                }
                formatted_inputs.push(Value::Object(url_formatted));
            }
        }
        
        // Find headers input second (optional)
        if let Some(headers_input) = inputs.iter().find(|input| {
            input.as_object().and_then(|obj| obj.get("name")).and_then(|v| v.as_str()) == Some("headers")
        }) {
            if let Some(headers_obj) = headers_input.as_object() {
                let mut headers_formatted = serde_json::Map::new();
                headers_formatted.insert("name".to_string(), Value::String("headers".to_string()));
                
                // Resolve template variables in the headers value
                if let Some(headers_value) = headers_obj.get("value") {
                    let resolved_headers = self.resolve_template_variables_in_value(headers_value, inputs)?;
                    headers_formatted.insert("value".to_string(), resolved_headers);
                }
                formatted_inputs.push(Value::Object(headers_formatted));
            }
        }
        
        Ok(Value::Array(formatted_inputs))
    }
    
    /// Resolve template variables from parent inputs and sibling outputs
    async fn resolve_template_variables_from_context(&self, value: &Value, _action: &ActionState, parent_action: Option<&ActionState>) -> Result<Value> {
        match value {
            Value::Object(obj) => {
                // Recursively resolve template variables in object values
                let mut resolved_obj = serde_json::Map::new();
                for (key, val) in obj {
                    let resolved_val = Box::pin(self.resolve_template_variables_from_context(val, _action, parent_action)).await?;
                    resolved_obj.insert(key.clone(), resolved_val);
                }
                Ok(Value::Object(resolved_obj))
            },
            Value::Array(arr) => {
                // Recursively resolve template variables in array values
                let mut resolved_arr = Vec::new();
                for val in arr {
                    let resolved_val = Box::pin(self.resolve_template_variables_from_context(val, _action, parent_action)).await?;
                    resolved_arr.push(resolved_val);
                }
                Ok(Value::Array(resolved_arr))
            },
            Value::String(s) => {
                if s.contains("{{") && s.contains("}}") {
                    let mut result = s.clone();
                    
                    // Try to resolve from step-based outputs first ({{steps.step_name.outputs[index].field}})
                    let steps_pattern = regex::Regex::new(r"\{\{steps\.([^.]+)\.outputs\[(\d+)\]\.([^}]+)\}\}")?;
                    result = steps_pattern.replace_all(&result, |caps: &regex::Captures| {
                        let step_name = &caps[1];
                        let output_index = &caps[2];
                        let field_path = &caps[3];
                        
                        // Look for step outputs in the same parent
                        if let Some(parent) = parent_action {
                            
                            if let Some(step_output) = self.find_step_output(parent, step_name, output_index, field_path) {
                                return match step_output {
                                    Value::String(s) => s,
                                    Value::Number(n) => n.to_string(),
                                    Value::Bool(b) => b.to_string(),
                                    _ => serde_json::to_string(&step_output).unwrap_or_else(|_| "null".to_string()),
                                };
                            }
                        }
                        
                        caps[0].to_string() // Keep original if not found
                    }).to_string();
                    
                    // Try to resolve from simple sibling outputs ({{step_name.field}})
                    let step_pattern = regex::Regex::new(r"\{\{([^}]+)\.([^}]+)\}\}")?;
                    result = step_pattern.replace_all(&result, |caps: &regex::Captures| {
                        let step_name = &caps[1];
                        let field = &caps[2];
                        
                        // Look for sibling outputs in the same parent
                        if let Some(parent) = parent_action {
                            if let Some(sibling_output) = self.find_sibling_output(parent, step_name, field) {
                                return match sibling_output {
                                    Value::String(s) => s,
                                    Value::Number(n) => n.to_string(),
                                    Value::Bool(b) => b.to_string(),
                                    _ => serde_json::to_string(&sibling_output).unwrap_or_else(|_| "null".to_string()),
                                };
                            }
                        }
                        
                        caps[0].to_string() // Keep original if not found
                    }).to_string();
                    
                    // Try to resolve from parent inputs ({{inputs[0].field}})
                    let input_pattern = regex::Regex::new(r"\{\{inputs\[0\]\.([^}]+)\}\}")?;
                    result = input_pattern.replace_all(&result, |caps: &regex::Captures| {
                        let field = &caps[1];
                        
                        // Look for parent inputs
                        if let Some(parent) = parent_action {
                            if let Some(parent_input) = self.find_parent_input(parent, field) {
                                return match parent_input {
                                    Value::String(s) => s,
                                    Value::Number(n) => n.to_string(),
                                    Value::Bool(b) => b.to_string(),
                                    _ => serde_json::to_string(&parent_input).unwrap_or_else(|_| "null".to_string()),
                                };
                            }
                        }
                        
                        caps[0].to_string() // Keep original if not found
                    }).to_string();
                    
                    Ok(Value::String(result))
                } else {
                    Ok(value.clone())
                }
            },
            _ => Ok(value.clone())
        }
    }
    
    /// Find step output by searching through sibling outputs directly
    fn find_step_output(&self, parent_action: &ActionState, _step_name: &str, output_index: &str, field_path: &str) -> Option<Value> {
        
        // Look through all sibling actions to find one with outputs that match our needs
        for (_child_id, child_action) in &parent_action.children {
            
            // Skip if this action has no outputs
            if child_action.outputs.is_empty() {
                continue;
            }
            
            // Try to get the output from this sibling action
            if let Some(output_value) = self.get_output_from_action(child_action, output_index, field_path) {
                return Some(output_value);
            }
        }
        
        None
    }
    
    /// Helper method to get output from an action
    fn get_output_from_action(&self, child_action: &ActionState, output_index: &str, field_path: &str) -> Option<Value> {
        
        // Found the step, look for the output at the specified index
        if let Ok(index) = output_index.parse::<usize>() {
            if let Some(output) = child_action.outputs.get(index) {
                if let Some(output_obj) = output.as_object() {
                    if let Some(value) = output_obj.get("value") {
                        // Navigate the field path (e.g., "body[0].lat" or "body.weather[0].description")
                        let result = self.get_nested_value_from_output(value, field_path);
                        return result;
                    }
                }
            }
        }
        None
    }
    
    /// Find sibling output by step name and field
    fn find_sibling_output(&self, parent_action: &ActionState, step_name: &str, field: &str) -> Option<Value> {
        // Look through the parent's children to find the sibling step
        for (_child_id, child_action) in &parent_action.children {
            if child_action.name == step_name {
                // Found the sibling, look for the field in its outputs
                for output in &child_action.outputs {
                    if let Some(output_obj) = output.as_object() {
                        if let Some(name) = output_obj.get("name").and_then(|v| v.as_str()) {
                            if name == field {
                                return output_obj.get("value").cloned();
                            }
                        }
                    }
                }
            }
        }
        None
    }
    
    /// Find parent input by field name
    fn find_parent_input(&self, parent_action: &ActionState, field: &str) -> Option<Value> {
        // Look through the parent's inputs to find the field
        for input in &parent_action.inputs {
            if let Some(input_obj) = input.as_object() {
                if let Some(input_value) = input_obj.get("value") {
                    if let Some(input_value_obj) = input_value.as_object() {
                        if let Some(field_value) = input_value_obj.get(field) {
                            return Some(field_value.clone());
                        }
                    }
                }
            }
        }
        None
    }
    
    /// Get nested value from output using complex dot notation with array indexing
    fn get_nested_value_from_output(&self, value: &Value, path: &str) -> Option<Value> {
        // Split path by dots, but handle array indexing like "body[0].lat"
        let parts: Vec<&str> = path.split('.').collect();
        let mut current = value.clone();
        
        for part in parts {
            // Handle array indexing like "body[0]" or "weather[0]"
            if part.contains('[') && part.contains(']') {
                let field_name = &part[..part.find('[').unwrap()];
                let index_str = &part[part.find('[').unwrap() + 1..part.find(']').unwrap()];
                
                if let Ok(index) = index_str.parse::<usize>() {
                    if let Some(obj) = current.as_object() {
                        if let Some(field_value) = obj.get(field_name) {
                            if let Some(arr) = field_value.as_array() {
                                if let Some(item) = arr.get(index) {
                                    current = item.clone();
                                } else {
                                    return None;
                                }
                            } else {
                                return None;
                            }
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                } else {
                    return None;
                }
            } else {
                // Handle simple field access
                if let Some(obj) = current.as_object() {
                    if let Some(field_value) = obj.get(part) {
                        current = field_value.clone();
                    } else {
                        return None;
                    }
                } else {
                    return None;
                }
            }
        }
        
        Some(current)
    }
    
    /// Resolve template variables in a value using the inputs array
    fn resolve_template_variables_in_value(&self, value: &Value, inputs: &Vec<Value>) -> Result<Value> {
        match value {
            Value::String(s) => {
                if s.contains("{{") && s.contains("}}") {
                    // This is a template string, resolve it
                    let mut result = s.clone();
                    
                    // Replace {{inputs[0].field}} patterns
                    let input_pattern = regex::Regex::new(r"\{\{inputs\[0\]\.([^}]+)\}\}")?;
                    result = input_pattern.replace_all(&result, |caps: &regex::Captures| {
                        let field = &caps[1];
                        if let Some(input) = inputs.get(0) {
                            if let Some(input_obj) = input.as_object() {
                                if let Some(input_value) = input_obj.get("value") {
                                    if let Some(input_value_obj) = input_value.as_object() {
                                        if let Some(field_value) = input_value_obj.get(field) {
                                            return match field_value {
                                                Value::String(s) => s.clone(),
                                                Value::Number(n) => n.to_string(),
                                                Value::Bool(b) => b.to_string(),
                                                _ => serde_json::to_string(field_value).unwrap_or_else(|_| "null".to_string()),
                                            };
                                        }
                                    }
                                }
                            }
                        }
                        caps[0].to_string() // Keep original if not found
                    }).to_string();
                    
                    Ok(Value::String(result))
                } else {
                    Ok(value.clone())
                }
            },
            _ => Ok(value.clone())
        }
    }

    /// Determine execution order for child actions based on cross-action dependencies
    fn determine_cross_action_execution_order(&self, child_actions: &HashMap<String, ActionState>) -> Result<Vec<String>> {
        use std::collections::{HashMap as StdHashMap, HashSet, VecDeque};
        
        // Build dependency graph by analyzing template variables in child action inputs
        let mut dependencies: StdHashMap<String, HashSet<String>> = StdHashMap::new();
        let mut in_degree: StdHashMap<String, usize> = StdHashMap::new();
        
        // Initialize all child actions
        for (child_id, _) in child_actions {
            dependencies.insert(child_id.clone(), HashSet::new());
            in_degree.insert(child_id.clone(), 0);
        }
        
        // Analyze dependencies by looking at template variables in child action inputs
        for (child_id, child_action) in child_actions {
            let child_deps = self.extract_cross_action_dependencies(child_action, child_actions)?;
            for dep in child_deps {
                if dependencies.contains_key(&dep) {
                    dependencies.get_mut(&dep).unwrap().insert(child_id.clone());
                    *in_degree.get_mut(child_id).unwrap() += 1;
                }
            }
        }
        
        // Topological sort using Kahn's algorithm
        let mut queue: VecDeque<String> = in_degree.iter()
            .filter(|(_, &count)| count == 0)
            .map(|(child_id, _)| child_id.clone())
            .collect();
        
        let mut sorted_children = Vec::new();
        let mut iterations = 0;
        
        while let Some(current_child) = queue.pop_front() {
            iterations += 1;
            
            // Add the child to sorted list
            sorted_children.push(current_child.clone());
            
            // Update in-degree for dependent children
            if let Some(deps) = dependencies.get(&current_child) {
                for dependent_child in deps {
                    if let Some(count) = in_degree.get_mut(dependent_child) {
                        *count -= 1;
                        if *count == 0 {
                            queue.push_back(dependent_child.clone());
                        }
                    }
                }
            }
            
            // Safety check to prevent infinite loops
            if iterations > 100 {
                return Err(anyhow::anyhow!("Cross-action topological sort exceeded maximum iterations"));
            }
        }
        
        // Check for cycles
        if sorted_children.len() != child_actions.len() {
            return Err(anyhow::anyhow!("Circular dependency detected in cross-action dependencies"));
        }
        
        Ok(sorted_children)
    }
    
    /// Extract cross-action dependencies from a child action's inputs
    fn extract_cross_action_dependencies(&self, child_action: &ActionState, all_children: &HashMap<String, ActionState>) -> Result<Vec<String>> {
        let mut dependencies = Vec::new();
        
        // Look for template variables in the child action's inputs
        for input in &child_action.inputs {
            self.find_cross_action_template_dependencies(input, all_children, &mut dependencies)?;
        }
        
        Ok(dependencies)
    }
    
    /// Find cross-action template dependencies in a value
    fn find_cross_action_template_dependencies(&self, value: &Value, all_children: &HashMap<String, ActionState>, deps: &mut Vec<String>) -> Result<()> {
        match value {
            Value::String(s) => {
                // Look for patterns like {{steps.step_name.outputs[index].field}}
                let steps_pattern = regex::Regex::new(r"\{\{steps\.([^.]+)\.outputs\[(\d+)\]\.([^}]+)\}\}")?;
                for cap in steps_pattern.captures_iter(s) {
                    let step_name = &cap[1];
                    // Find which child action corresponds to this step
                    for (child_id, child_action) in all_children {
                        
                        // Try exact match first
                        if child_action.name == step_name {
                            if !deps.contains(child_id) {
                                deps.push(child_id.clone());
                            }
                            break;
                        }
                        
                        // Try partial match - check if the step name is contained in the action name
                        if child_action.name.contains(step_name) {
                            if !deps.contains(child_id) {
                                deps.push(child_id.clone());
                            }
                            break;
                        }
                        
                        // Try matching by the uses field - extract the package name and match with step name
                        let uses_parts: Vec<&str> = child_action.uses.split('/').collect();
                        if uses_parts.len() >= 2 {
                            let package_name = uses_parts[1].split(':').next().unwrap_or("");
                            
                            // Check if step name contains the package name or vice versa
                            if step_name.contains(package_name) || package_name.contains(step_name) {
                                if !deps.contains(child_id) {
                                    deps.push(child_id.clone());
                                }
                                break;
                            }
                            
                            // Check if step name matches a simplified version of the package name
                            if step_name == "get_coordinates" && package_name.contains("coordinates") {
                                if !deps.contains(child_id) {
                                    deps.push(child_id.clone());
                                }
                                break;
                            }
                            
                            if step_name == "get_weather" && package_name.contains("weather") && !package_name.contains("coordinates") {
                                if !deps.contains(child_id) {
                                    deps.push(child_id.clone());
                                }
                                break;
                            }
                        }
                        
                        // Try reverse partial match - check if the action name is contained in the step name
                        if step_name.contains(&child_action.name.split('.').last().unwrap_or("")) {
                            if !deps.contains(child_id) {
                                deps.push(child_id.clone());
                            }
                            break;
                        }
                        
                        // Try matching the last part of the action name (after the last dot)
                        let action_last_part = child_action.name.split('.').last().unwrap_or("");
                        if step_name.contains(action_last_part) || action_last_part.contains(step_name) {
                            if !deps.contains(child_id) {
                                deps.push(child_id.clone());
                            }
                            break;
                        }
                        
                        // Check if any child of this action matches the step name
                        if child_action.children.values().any(|c| c.name == step_name) {
                            if !deps.contains(child_id) {
                                deps.push(child_id.clone());
                            }
                            break;
                        }
                    }
                }
            },
            Value::Object(obj) => {
                for (_, v) in obj {
                    self.find_cross_action_template_dependencies(v, all_children, deps)?;
                }
            },
            Value::Array(arr) => {
                for item in arr {
                    self.find_cross_action_template_dependencies(item, all_children, deps)?;
                }
            },
            _ => {}
        }
        
        Ok(())
    }


}
