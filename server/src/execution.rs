use anyhow::{Result, bail, Context};
use serde_json::{Value, json, Map};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, process::Command};
use tokio::sync::mpsc;
use std::path::Path;
use std::collections::{HashSet, HashMap};
use which::which;

use crate::models::{ShManifest, ShKind, HubClient, StepSpec};

// Constants
const ST_MARKER: &str = "::starthub:state::";

// Data flow edge representing a variable dependency between steps
#[derive(Debug, Clone, serde::Serialize)]
struct DataFlowEdge {
    from_step: String,      // Source step name (or "inputs" for initial inputs)
    to_step: String,        // Destination step name
    variable_name: String,  // The variable name being passed
    source_path: String,    // Path in source (e.g., "outputs.lat" or "open_weather_config.location_name")
    target_path: String,    // Path in target (e.g., "lat" or "location_name")
}

// Execution state to track inputs and outputs of each step
#[derive(Debug, Clone, serde::Serialize)]
struct ExecutionState {
    inputs: HashMap<String, Value>,
    steps: Vec<StepState>,
    data_flow: Vec<DataFlowEdge>, // Global data flow mapping
}

#[derive(Debug, Clone, serde::Serialize)]
struct StepState {
    id: String,
    original_name: String, // Original step name from the composition
    uses: String,
    kind: String,
    ref_: String,
    inputs: HashMap<String, Value>,
    outputs: HashMap<String, Value>,
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
        
        // 1. Download the manifest of the action
        let manifests = self.fetch_all_action_manifests(action_ref).await?;
        if manifests.is_empty() {
            return Err(anyhow::anyhow!("No action definition found for: {}", action_ref));
        }
        
        let main_manifest = &manifests[0];
        
        // 2. Apply topological sorting to composition steps BEFORE flattening
        let sorted_composition_steps = self.topological_sort_composition_steps(main_manifest).await?;
        
        // 3. Flatten the topologically sorted composition steps into atomic steps
        let execution_steps = self.flatten_sorted_steps_to_atomic(sorted_composition_steps, &inputs, action_ref).await?;
    
        // 4. Prepare steps and store them in state
        let mut execution_state = self.prepare_atomic_steps(&execution_steps, &inputs).await?;
        
        // Print the execution steps
        println!("Execution state: {:#?}", execution_state);
        
        // 5. Execute the prepared steps
        self.execute_prepared_steps(&mut execution_state).await
    }

    async fn convert_composite_steps_to_execution(&self, manifest: &ShManifest, _inputs: &HashMap<String, Value>, action_ref: &str) -> Result<Vec<StepSpec>> {
        let mut execution_steps = Vec::new();
        
        // If this is a simple action (WASM/Docker) with no steps, create a single step
        if manifest.steps.is_empty() {
            let step_kind = match manifest.kind {
                Some(crate::models::ShKind::Wasm) => "wasm",
                Some(crate::models::ShKind::Docker) => "docker",
                _ => "wasm", // Default fallback
            };
            
                let step = StepSpec {
                    id: "main".to_string(),
                kind: step_kind.to_string(),
                    ref_: action_ref.to_string(),
                    args: vec![],
                    env: std::collections::HashMap::new(),
                    workdir: None,
                    network: None,
                    entry: None,
                    mounts: vec![],
                step_definition: None,
                calling_step_definition: None,
            };
            
            execution_steps.push(step);
            return Ok(execution_steps);
        }
        
        // Process composite action steps
        for (step_id, step_data) in &manifest.steps {
            
            // Parse step data
            let step_obj = step_data.as_object()
                .ok_or_else(|| anyhow::anyhow!("Step {} is not an object", step_id))?;
            
            // Extract the 'uses' field
            let uses_data = step_obj.get("uses")
                .ok_or_else(|| anyhow::anyhow!("Step {} missing 'uses' field", step_id))?
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Step {} 'uses' field is not a string", step_id))?;
            
            // Fetch the step's manifest to determine its actual kind
            let step_manifests = self.fetch_all_action_manifests(uses_data).await?;
            let step_manifest = step_manifests.first()
                .ok_or_else(|| anyhow::anyhow!("No manifest found for step: {}", uses_data))?;
            
            // Check if this is a composition step that needs to be expanded
            match step_manifest.kind {
                Some(crate::models::ShKind::Composition) => {
                    // Recursively expand composition steps into their atomic steps
                    let expanded_steps = Box::pin(self.convert_composite_steps_to_execution(step_manifest, _inputs, uses_data)).await?;
                    execution_steps.extend(expanded_steps);
                },
                _ => {
                    // Create atomic step (WASM/Docker)
                    let step_kind = match step_manifest.kind {
                        Some(crate::models::ShKind::Wasm) => "wasm",
                        Some(crate::models::ShKind::Docker) => "docker",
                        _ => "wasm", // Default fallback
                    };
                    
            let execution_step = StepSpec {
                id: step_id.clone(),
                kind: step_kind.to_string(),
                ref_: uses_data.to_string(),
                args: vec![],
                env: std::collections::HashMap::new(),
                workdir: None,
                network: None,
                entry: None,
                mounts: vec![],
                        step_definition: Some(step_data.clone()),
                        calling_step_definition: Some(step_data.clone()),
            };
            
            execution_steps.push(execution_step);
        }
            }
        }
        
        
        Ok(execution_steps)
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
    
    async fn flatten_sorted_steps_to_atomic(&self, sorted_steps: Vec<(String, serde_json::Value)>, _inputs: &HashMap<String, Value>, _action_ref: &str) -> Result<Vec<StepSpec>> {
        let mut execution_steps = Vec::new();
        
        for (step_id, step_data) in sorted_steps {
            // Parse step data
            let step_obj = step_data.as_object()
                .ok_or_else(|| anyhow::anyhow!("Step {} is not an object", step_id))?;
            
            // Extract the 'uses' field
            let uses_data = step_obj.get("uses")
                .ok_or_else(|| anyhow::anyhow!("Step {} missing 'uses' field", step_id))?
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Step {} 'uses' field is not a string", step_id))?;
            
            // Fetch the step's manifest to determine its actual kind
            let step_manifests = self.fetch_all_action_manifests(uses_data).await?;
            let step_manifest = step_manifests.first()
                .ok_or_else(|| anyhow::anyhow!("No manifest found for step: {}", uses_data))?;
            
            // Check if this is a composition step that needs to be expanded
            match step_manifest.kind {
                Some(crate::models::ShKind::Composition) => {
                    // Recursively expand composition steps into their atomic steps
                    let expanded_steps = Box::pin(self.convert_composite_steps_to_execution(step_manifest, _inputs, uses_data)).await?;
                    execution_steps.extend(expanded_steps);
                },
                _ => {
                    // Create atomic step (WASM/Docker)
                    let step_kind = match step_manifest.kind {
                        Some(crate::models::ShKind::Wasm) => "wasm",
                        Some(crate::models::ShKind::Docker) => "docker",
                        _ => "wasm", // Default fallback
                    };
                    
                    
                    // Fetch the target action's step definition
                    let target_step_definition = if let Some(target_step) = step_manifest.steps.get(&step_id) {
                        Some(target_step.clone())
                    } else {
                        None
                    };
                    
                    let execution_step = StepSpec {
                        id: step_id.clone(),
                        kind: step_kind.to_string(),
                        ref_: uses_data.to_string(),
                        args: vec![],
                        env: std::collections::HashMap::new(),
                        workdir: None,
                        network: None,
                        entry: None,
                        mounts: vec![],
                        step_definition: target_step_definition,
                        calling_step_definition: Some(step_data.clone()), // This should be the composition step definition
                    };
                    
                    execution_steps.push(execution_step);
                }
            }
        }
        
        Ok(execution_steps)
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
                            // Extract step name from template (e.g., "get_coordinates.coordinates.lat" -> "get_coordinates")
                            if let Some(dot_pos) = template.find('.') {
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
    
    async fn prepare_atomic_steps(&self, steps: &[StepSpec], inputs: &HashMap<String, Value>) -> Result<ExecutionState> {
        // Parse stringified JSON values in inputs
        let mut parsed_inputs = HashMap::new();
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
        
        // Initialize execution state with parsed inputs
        let mut execution_state = ExecutionState {
            inputs: parsed_inputs,
            steps: Vec::new(),
            data_flow: Vec::new(),
        };
        
        // Prepare steps without executing them
        for step in steps.iter() {
            // Generate UUID for this step
            let step_id = uuid::Uuid::new_v4().to_string();
            
            // Build parameters for this step using the execution state
            let step_params = self.build_step_parameters_from_state(step, &execution_state).await?;
            
            // Analyze template variables to build data flow edges
            let data_flow_edges = self.analyze_data_flow_edges(step, &step.id)?;
            execution_state.data_flow.extend(data_flow_edges);
            
            // Store the step state in execution state (without outputs yet)
            let step_state = StepState {
                id: step_id,
                original_name: step.id.clone(), // Use the original step name from the composition
                uses: step.ref_.clone(),
                kind: step.kind.clone(),
                ref_: step.ref_.clone(),
                inputs: step_params.as_object().unwrap().clone().into_iter().collect(),
                outputs: HashMap::new(), // Empty outputs initially
            };
            execution_state.steps.push(step_state);
        }
        
        Ok(execution_state)
    }
    
    fn analyze_data_flow_edges(&self, step: &StepSpec, step_name: &str) -> Result<Vec<DataFlowEdge>> {
        let mut data_flow_edges = Vec::new();
        
        // Analyze the calling step definition to find template variables
        if let Some(calling_step_def) = &step.calling_step_definition {
            // Focus on the 'inputs' field of the step definition
            if let Some(inputs_obj) = calling_step_def.get("inputs").and_then(|v| v.as_object()) {
                for (input_key, input_value) in inputs_obj {
                    self.analyze_template_variables_for_data_flow(input_value, step_name, input_key, &mut data_flow_edges)?;
                }
            }
        }
        
        Ok(data_flow_edges)
    }
    
    fn analyze_template_variables_for_data_flow(&self, value: &Value, target_step: &str, input_key: &str, data_flow_edges: &mut Vec<DataFlowEdge>) -> Result<()> {
        self.analyze_template_variables_for_data_flow_recursive(value, target_step, input_key, data_flow_edges)
    }
    
    fn analyze_template_variables_for_data_flow_recursive(&self, value: &Value, target_step: &str, current_path: &str, data_flow_edges: &mut Vec<DataFlowEdge>) -> Result<()> {
        match value {
            Value::String(s) => {
                // Check if this string contains template variables
                if s.contains("{{") {
                    // Extract all template variables in this string
                    let re = regex::Regex::new(r"\{\{([^}]+)\}\}")?;
                    let mut has_inputs = false;
                    let mut step_dependencies = Vec::new();
                    
                    for cap in re.captures_iter(s) {
                        if let Some(match_str) = cap.get(1) {
                            let template = match_str.as_str();
                            
                            if template.starts_with("inputs.") {
                                has_inputs = true;
                            } else if let Some(dot_pos) = template.find('.') {
                                let step_name = &template[..dot_pos];
                                if !step_dependencies.contains(&step_name.to_string()) {
                                    step_dependencies.push(step_name.to_string());
                                }
                            }
                        }
                    }
                    
                    // Create edges based on what we found
                    if has_inputs {
                        data_flow_edges.push(DataFlowEdge {
                            from_step: "inputs".to_string(),
                            to_step: target_step.to_string(),
                            variable_name: current_path.to_string(),
                            source_path: "inputs".to_string(),
                            target_path: current_path.to_string(),
                        });
                    }
                    
                    for step_name in step_dependencies {
                        data_flow_edges.push(DataFlowEdge {
                            from_step: step_name,
                            to_step: target_step.to_string(),
                            variable_name: current_path.to_string(),
                            source_path: "outputs".to_string(),
                            target_path: current_path.to_string(),
                        });
                    }
                }
            },
            Value::Object(obj) => {
                for (key, v) in obj {
                    let new_path = if current_path.is_empty() {
                        key.clone()
                    } else {
                        format!("{}.{}", current_path, key)
                    };
                    self.analyze_template_variables_for_data_flow_recursive(v, target_step, &new_path, data_flow_edges)?;
                }
            },
            Value::Array(arr) => {
                for (index, item) in arr.iter().enumerate() {
                    let new_path = if current_path.is_empty() {
                        format!("[{}]", index)
                    } else {
                        format!("{}[{}]", current_path, index)
                    };
                    self.analyze_template_variables_for_data_flow_recursive(item, target_step, &new_path, data_flow_edges)?;
                }
            },
            _ => {}
        }
        
        Ok(())
    }
    
    async fn execute_prepared_steps(&self, execution_state: &mut ExecutionState) -> Result<Value> {
        let total_steps = execution_state.steps.len();
        for (i, step_state) in execution_state.steps.iter_mut().enumerate() {
            println!("Executing step {}/{}: {}", i + 1, total_steps, step_state.id);
            
            // Execute the step using the information stored in the step state
            let result = match step_state.kind.as_str() {
                "wasm" => {
                    let step_params = Value::Object(step_state.inputs.clone().into_iter().collect());
                    println!("📤 WASM inputs for step '{}': {}", step_state.id, serde_json::to_string_pretty(&step_params).unwrap_or_else(|_| "{}".to_string()));
                    
                    // Create a temporary StepSpec for the run_wasm_step function
                    let temp_step = StepSpec {
                        id: step_state.id.clone(),
                        kind: step_state.kind.clone(),
                        ref_: step_state.ref_.clone(),
                        args: vec![],
                        env: std::collections::HashMap::new(),
                        workdir: None,
                        network: None,
                        entry: None,
                        mounts: vec![],
                        step_definition: None,
                        calling_step_definition: None,
                    };
                    
                    self.run_wasm_step(&temp_step, None, &step_params).await?
                },
                "docker" => {
                    let step_params = Value::Object(step_state.inputs.clone().into_iter().collect());
                    println!("📤 Docker inputs for step '{}': {}", step_state.id, serde_json::to_string_pretty(&step_params).unwrap_or_else(|_| "{}".to_string()));
                    
                    // Create a temporary StepSpec for the run_docker_step function
                    let temp_step = StepSpec {
                        id: step_state.id.clone(),
                        kind: step_state.kind.clone(),
                        ref_: step_state.ref_.clone(),
                        args: vec![],
                        env: std::collections::HashMap::new(),
                        workdir: None,
                        network: None,
                        entry: None,
                        mounts: vec![],
                        step_definition: None,
                        calling_step_definition: None,
                    };
                    
                    self.run_docker_step(&temp_step, None, &step_params).await?
                },
                _ => return Err(anyhow::anyhow!("Unknown step kind: {}", step_state.kind)),
            };
            
            // Store the outputs in the step state
            step_state.outputs = result.as_object().unwrap().clone().into_iter().collect();
        }
        
        // Return the final execution state
        Ok(serde_json::to_value(execution_state)?)
    }

    async fn fetch_all_action_manifests(&self, action_ref: &str) -> Result<Vec<ShManifest>> {
        let mut visited = HashSet::new();
        self.fetch_all_action_manifests_recursive(action_ref, &mut visited).await
    }

    async fn fetch_all_action_manifests_recursive(
        &self,
        action_ref: &str,
        visited: &mut HashSet<String>
    ) -> Result<Vec<ShManifest>> {
        if visited.contains(action_ref) {
            return Ok(vec![]);
        }
        visited.insert(action_ref.to_string());
        
        // Construct storage URL for starthub-lock.json (hardcoded pattern)
        // Convert action_ref from "org/name:version" to "org/name/version" format
        let url_path = action_ref.replace(":", "/");
        let storage_url = format!(
            "https://api.starthub.so/storage/v1/object/public/artifacts/{}/starthub-lock.json",
            url_path
        );
        
        // Download and parse starthub-lock.json
        let manifest = self.client.download_starthub_lock(&storage_url).await?;
        let mut all_manifests = vec![manifest.clone()];
        
        // If it's a WASM or Docker action, download and extract artifacts
        if let Some(kind) = &manifest.kind {
            match kind {
                ShKind::Wasm | ShKind::Docker => {
                    self.download_and_extract_artifacts(action_ref).await?;
                }
                ShKind::Composition => {
                    // For compositions, recursively process each step
                    for (_step_id, step_data) in &manifest.steps {
                        if let Some(step_obj) = step_data.as_object() {
                            if let Some(uses_data) = step_obj.get("uses").and_then(|v| v.as_str()) {
                                if let Ok(step_manifests) = Box::pin(self.fetch_all_action_manifests_recursive(uses_data, visited)).await {
                                    all_manifests.extend(step_manifests);
                                }
                            }
                        }
                    }
                }
            }
        }
        
        Ok(all_manifests)
    }

    async fn download_and_extract_artifacts(&self, action_ref: &str) -> Result<()> {
        // Construct storage URL for artifacts zip file
        // Convert action_ref from "org/name:version" to "org/name/version" format
        let url_path = action_ref.replace(":", "/");
        let artifacts_url = format!(
            "https://api.starthub.so/storage/v1/object/public/artifacts/{}/artifact.zip",
            url_path
        );
        
        // Create action-specific cache directory
        let action_cache_dir = self.cache_dir.join(action_ref);
        std::fs::create_dir_all(&action_cache_dir)?;
        
        // Download the artifacts zip file
        let response = reqwest::get(&artifacts_url).await?;
        if !response.status().is_success() {
            bail!("Failed to download artifacts from {}", artifacts_url);
        }
        
        let zip_data = response.bytes().await?;
        
        // Extract the zip file
        let cursor = std::io::Cursor::new(zip_data);
        let mut archive = zip::ZipArchive::new(cursor)?;
        archive.extract(&action_cache_dir)?;
        
        
        Ok(())
    }



    async fn build_step_parameters_from_state(&self, step: &StepSpec, execution_state: &ExecutionState) -> Result<Value> {
        // Use the calling step definition for template processing (this has the correct template variables)
        if let Some(calling_step_def) = &step.calling_step_definition {
            return self.process_step_definition_from_state(calling_step_def, execution_state).await;
        }
        
        // Fallback: for simple actions without step definitions, use initial inputs directly
        let mut params = Map::new();
        for (key, value) in &execution_state.inputs {
            params.insert(key.clone(), value.clone());
        }
        
        Ok(Value::Object(params))
    }

    async fn process_step_definition_from_state(&self, step_def: &Value, execution_state: &ExecutionState) -> Result<Value> {
        
        let step_obj = step_def.as_object()
            .ok_or_else(|| anyhow::anyhow!("Step definition is not an object"))?;
        
        // Get the inputs object from the step definition (this is from the calling action)
        let inputs_obj = step_obj.get("inputs")
            .and_then(|v| v.as_object())
            .ok_or_else(|| anyhow::anyhow!("Step definition missing 'inputs' object"))?;
        
        let mut module_params = Map::new();
        
        // Process each input key-value pair directly
        // The keys should match what the target action expects
        // The values should be processed template variables from the calling action
        for (input_key, input_value) in inputs_obj {
            let processed_value = self.process_template_variable_from_state(input_value, execution_state)?;
            module_params.insert(input_key.clone(), processed_value);
        }
        
        
        Ok(Value::Object(module_params))
    }

    fn process_template_variable_from_state(&self, template: &Value, execution_state: &ExecutionState) -> Result<Value> {
        match template {
            Value::String(template_str) => {
                // Process template string like "{{inputs.open_weather_config.location_name}}"
                let mut result = template_str.clone();
                
                // Replace {{inputs.*}} patterns
                let input_pattern = regex::Regex::new(r"\{\{inputs\.([^}]+)\}\}")?;
                result = input_pattern.replace_all(&result, |caps: &regex::Captures| {
                    let path = &caps[1];
                    if let Some(value) = self.get_nested_value(&execution_state.inputs, path) {
                        match value {
                            Value::String(s) => s.clone(),
                            Value::Number(n) => n.to_string(),
                            Value::Bool(b) => b.to_string(),
                            _ => serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string()),
                        }
                    } else {
                        caps[0].to_string()
                    }
                }).to_string();
                
                // Replace {{step_name.inputs.*}} and {{step_name.outputs.*}} patterns
                let step_pattern = regex::Regex::new(r"\{\{([^.]+)\.(inputs|outputs)\.([^}]+)\}\}")?;
                result = step_pattern.replace_all(&result, |caps: &regex::Captures| {
                    let step_name = &caps[1];
                    let section = &caps[2];
                    let path = &caps[3];
                    
                    // Find step by original name (this is the key fix!)
                    let step_state = execution_state.steps.iter()
                        .find(|step| step.original_name == step_name);
                    
                    if let Some(step_state) = step_state {
                        let target_map = match section {
                            "inputs" => &step_state.inputs,
                            "outputs" => &step_state.outputs,
                            _ => return caps[0].to_string(),
                        };
                        if let Some(value) = self.get_nested_value(target_map, path) {
                            match value {
                                Value::String(s) => s.clone(),
                                Value::Number(n) => n.to_string(),
                                Value::Bool(b) => b.to_string(),
                                _ => serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string()),
                            }
                        } else {
                            caps[0].to_string()
                        }
                    } else {
                        caps[0].to_string()
                    }
                }).to_string();
                
                Ok(Value::String(result))
            },
            Value::Object(obj) => {
                // Process object templates recursively
                let mut processed_obj = Map::new();
                for (key, value) in obj {
                    processed_obj.insert(key.clone(), self.process_template_variable_from_state(value, execution_state)?);
                }
                Ok(Value::Object(processed_obj))
            },
            Value::Array(arr) => {
                // Process array templates recursively
                let mut processed_arr = Vec::new();
                for item in arr {
                    processed_arr.push(self.process_template_variable_from_state(item, execution_state)?);
                }
                Ok(Value::Array(processed_arr))
            },
            other => Ok(other.clone()),
        }
    }


    fn get_nested_value(&self, inputs: &HashMap<String, Value>, path: &str) -> Option<Value> {
        let parts: Vec<&str> = path.split('.').collect();
        let mut current = inputs.get(parts[0])?.clone();
        
        for part in parts.iter().skip(1) {
            if let Some(obj) = current.as_object() {
                current = obj.get(*part)?.clone();
            } else {
                return None;
            }
        }
        
        Some(current)
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

}
