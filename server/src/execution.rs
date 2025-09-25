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

// Execution state to track inputs and outputs of each step
#[derive(Debug, Clone, serde::Serialize)]
struct ExecutionState {
    inputs: HashMap<String, Value>,
    steps: Vec<StepState>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct StepState {
    id: String,
    uses: String,
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
        
        // Recursively resolve all manifests and their dependencies
        let manifests = self.fetch_all_action_manifests(action_ref).await?;
        if manifests.is_empty() {
            return Err(anyhow::anyhow!("No action definition found for: {}", action_ref));
        }
        
        let main_manifest = &manifests[0];
        
        // Handle different action types
        match main_manifest.kind {
            Some(crate::models::ShKind::Wasm) | Some(crate::models::ShKind::Docker) => {
                // Simple action - create a single step
                let step = StepSpec {
                    id: "main".to_string(),
                    kind: match main_manifest.kind {
                        Some(crate::models::ShKind::Wasm) => "wasm".to_string(),
                        Some(crate::models::ShKind::Docker) => "docker".to_string(),
                        _ => unreachable!(),
                    },
                    ref_: action_ref.to_string(),
                    args: vec![],
                    env: std::collections::HashMap::new(),
                    workdir: None,
                    network: None,
                    entry: None,
                    mounts: vec![],
                    step_definition: None,
                };
                
                
                // Execute the single step
                self.execute_all_steps(&[step], &inputs).await
            },
            Some(crate::models::ShKind::Composition) | None => {
                // Composite action - process steps
                
                // Convert steps to execution format
                let execution_steps = self.convert_composite_steps_to_execution(main_manifest, &inputs).await?;
                
                
                // Execute all steps in order
                self.execute_all_steps(&execution_steps, &inputs).await
            }
        }
    }

    async fn convert_composite_steps_to_execution(&self, manifest: &ShManifest, _inputs: &HashMap<String, Value>) -> Result<Vec<StepSpec>> {
        let mut execution_steps = Vec::new();
        
        for (step_id, step_data) in &manifest.steps {
            
            // Parse step data
            let step_obj = step_data.as_object()
                .ok_or_else(|| anyhow::anyhow!("Step {} is not an object", step_id))?;
            
            // Extract the 'uses' field
            let uses_data = step_obj.get("uses")
                .ok_or_else(|| anyhow::anyhow!("Step {} missing 'uses' field", step_id))?
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Step {} 'uses' field is not a string", step_id))?;
            
            // Determine step kind based on the action being used
            let step_kind = if uses_data.contains("http-get-wasm") {
                "wasm"
            } else if uses_data.contains("docker") {
                "docker"
            } else {
                // Default to WASM for now
                "wasm"
            };
            
            // Create execution step
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
            };
            
            execution_steps.push(execution_step);
        }
        
        Ok(execution_steps)
    }
    
    
    async fn execute_all_steps(&self, steps: &[StepSpec], inputs: &HashMap<String, Value>) -> Result<Value> {
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
        };
        
        for step in steps {
            // Generate UUID for this step
            let step_id = uuid::Uuid::new_v4().to_string();
            
            // Build parameters for this step using the execution state
            let step_params = self.build_step_parameters_from_state(step, &execution_state).await?;
            
            // Log execution state and step inputs
            println!("📊 Execution State:");
            println!("  Inputs: {}", serde_json::to_string_pretty(&execution_state.inputs)?);
            println!("  Steps completed: {}", execution_state.steps.len());
            println!("📤 Step '{}' inputs:", step.id);
            println!("{}", serde_json::to_string_pretty(&step_params)?);
            
            // Execute the step
            let result = match step.kind.as_str() {
                "wasm" => self.run_wasm_step(step, None, &step_params).await?,
                "docker" => self.run_docker_step(step, None, &step_params).await?,
                _ => return Err(anyhow::anyhow!("Unknown step kind: {}", step.kind)),
            };
            
            // Log step output
            println!("📥 Step '{}' outputs:", step.id);
            println!("{}", serde_json::to_string_pretty(&result)?);
            
            // Store the step state in execution state
            let step_state = StepState {
                id: step_id,
                uses: step.ref_.clone(),
                inputs: step_params.as_object().unwrap().clone().into_iter().collect(),
                outputs: result.as_object().unwrap().clone().into_iter().collect(),
            };
            execution_state.steps.push(step_state);
        }
        
        // Log the final execution state
        println!("📊 Final Execution State:");
        println!("{}", serde_json::to_string_pretty(&execution_state)?);
        
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
        println!("📁 Cache directory: {}", action_cache_dir.display());
        
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
        
        println!("✅ Artifacts extracted to: {}", action_cache_dir.display());
        
        Ok(())
    }



    async fn build_step_parameters_from_state(&self, step: &StepSpec, execution_state: &ExecutionState) -> Result<Value> {
        // If we have a step definition, process it to build correct parameters
        if let Some(step_def) = &step.step_definition {
            return self.process_step_definition_from_state(step_def, execution_state).await;
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
        
        // Get the target module reference
        let uses_ref = step_obj.get("uses")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Step definition missing 'uses' field"))?;
        
        // Fetch the target module's manifest to understand its input structure
        let target_manifests = self.fetch_all_action_manifests(uses_ref).await?;
        let target_manifest = target_manifests.first()
            .ok_or_else(|| anyhow::anyhow!("No manifest found for target module: {}", uses_ref))?;
        
        // Get the inputs array from the step definition
        let inputs_array = step_obj.get("inputs")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("Step definition missing 'inputs' array"))?;
        
        let mut module_params = Map::new();
        
        // Get the target module's input field names from its manifest
        let target_inputs = &target_manifest.inputs;
        
                // Process each input in the array and map to target module's expected field names by type
        for input_item in inputs_array.iter() {
            if let Some(input_obj) = input_item.as_object() {
                // Get the type and value
                if let (Some(input_type), Some(input_value)) = (input_obj.get("type"), input_obj.get("value")) {
                    let processed_value = self.process_template_variable_from_state(input_value, execution_state)?;
                    
                    // Find the field in target manifest that matches this input type
                    if let Some(input_type_str) = input_type.as_str() {
                        for (field_name, field_def) in target_inputs {
                            if let Some(field_type) = field_def.get("type").and_then(|t| t.as_str()) {
                                if field_type == input_type_str {
                                    module_params.insert(field_name.clone(), processed_value);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
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
                    
                    // Find step by name (could be step ID or uses reference)
                    let step_state = execution_state.steps.iter()
                        .find(|step| step.id == step_name || step.uses == step_name);
                    
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
