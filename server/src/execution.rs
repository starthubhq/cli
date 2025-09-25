use anyhow::{Result, bail, Context};
use serde_json::{Value, json, Map};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, process::Command};
use tokio::sync::mpsc;
use std::path::Path;
use std::collections::{HashSet, HashMap};
use which::which;

use crate::models::{ShManifest, ShKind, HubClient, StepSpec, ActionPlan};

// Constants
const ST_MARKER: &str = "::starthub:state::";

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
        println!("🚀 Starting execution of action: {}", action_ref);
        
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
        println!("✓ Main action: {} (version: {})", main_manifest.name, main_manifest.version);
        
        // For simple actions, create a single step
        let mut all_steps = Vec::new();
        
        if main_manifest.steps.is_empty() {
            // Simple action - create a single step
            let step = match main_manifest.kind {
                Some(crate::models::ShKind::Wasm) => {
                    StepSpec {
                        id: "main".to_string(),
                        kind: "wasm".to_string(),
                        ref_: action_ref.to_string(),
                        args: vec![],
                        env: std::collections::HashMap::new(),
                        workdir: None,
                        network: None,
                        entry: None,
                        mounts: vec![],
                    }
                },
                Some(crate::models::ShKind::Docker) => {
                    StepSpec {
                        id: "main".to_string(),
                        kind: "docker".to_string(),
                        ref_: action_ref.to_string(),
                        args: vec![],
                        env: std::collections::HashMap::new(),
                        workdir: None,
                        network: None,
                        entry: None,
                        mounts: vec![],
                    }
                },
                _ => return Err(anyhow::anyhow!("Unsupported action kind: {:?}", main_manifest.kind)),
            };
            all_steps.push(step);
        } else {
            // Composite action - use existing logic
        let plan = self.build_execution_plan(main_manifest)?;
            all_steps = plan.steps;
        }
        
        println!("✓ Total steps resolved: {}", all_steps.len());
        println!("📋 Execution order: {:?}", all_steps.iter().map(|s| &s.id).collect::<Vec<_>>());
        
        // Execute all steps in order
        self.execute_all_steps(&all_steps, &inputs).await
    }

    
    fn topological_sort_steps(&self, steps: &[StepSpec]) -> Result<Vec<StepSpec>> {
        use std::collections::{HashMap, HashSet, VecDeque};
        
        let mut dependencies: HashMap<String, HashSet<String>> = HashMap::new();
        let mut dependents: HashMap<String, HashSet<String>> = HashMap::new();
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        
        // Initialize all steps
        for step in steps {
            dependencies.insert(step.id.clone(), HashSet::new());
            dependents.insert(step.id.clone(), HashSet::new());
            in_degree.insert(step.id.clone(), 0);
        }
        
        // For now, assume no dependencies between steps (they're all base actions)
        // In a real implementation, we'd analyze step inputs/outputs to build the dependency graph
        
        // Kahn's algorithm for topological sort
        let mut queue = VecDeque::new();
        let mut result = Vec::new();
        
        // Add all steps with zero in-degree to queue
        for (step_id, &degree) in &in_degree {
            if degree == 0 {
                queue.push_back(step_id.clone());
            }
        }
        
        while let Some(step_id) = queue.pop_front() {
            result.push(step_id.clone());
            
            // Process dependents
            if let Some(deps) = dependents.get(&step_id) {
                for dep in deps {
                    if let Some(degree) = in_degree.get_mut(dep) {
                        *degree -= 1;
                        if *degree == 0 {
                            queue.push_back(dep.clone());
                        }
                    }
                }
            }
        }
        
        // Convert step IDs back to StepSpecs
        let mut sorted_steps = Vec::new();
        for step_id in result {
            if let Some(step) = steps.iter().find(|s| s.id == step_id) {
                sorted_steps.push(step.clone());
            }
        }
        
        Ok(sorted_steps)
    }
    
    async fn execute_all_steps(&self, steps: &[StepSpec], inputs: &HashMap<String, Value>) -> Result<Value> {
        let mut state = serde_json::Value::Object(inputs.clone().into_iter().collect());
        
        for step in steps {
            println!("🔧 Executing step: {}", step.id);
            
            // Build parameters for this step
            let step_params = self.build_step_parameters(step, &state, inputs)?;
            
            // Execute the step (download/extraction is handled by run_wasm_step/run_docker_step)
            let result = match step.kind.as_str() {
                "wasm" => self.run_wasm_step(step, None, &step_params).await?,
                "docker" => self.run_docker_step(step, None, &step_params).await?,
                _ => return Err(anyhow::anyhow!("Unknown step kind: {}", step.kind)),
            };
            
            // Update state with step result
            self.deep_merge(&mut state, result);
        }
        
        Ok(state)
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
                    for step in &manifest.steps {
                        if let Ok(step_manifests) = Box::pin(self.fetch_all_action_manifests_recursive(&step.uses.name, visited)).await {
                            all_manifests.extend(step_manifests);
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
        
        println!("Downloaded and extracted artifacts for {}", action_ref);
        Ok(())
    }

    fn build_execution_plan(&self, manifest: &ShManifest) -> Result<ActionPlan> {
        // Build dependency graph and sort steps
        let sorted_steps = self.resolve_step_dependencies(manifest)?;
        
        Ok(ActionPlan {
            steps: sorted_steps,
            workdir: None,
        })
    }

    fn resolve_step_dependencies(&self, manifest: &ShManifest) -> Result<Vec<StepSpec>> {
        use std::collections::{HashMap, HashSet, VecDeque};
        
        let mut dependencies: HashMap<String, HashSet<String>> = HashMap::new();
        let mut dependents: HashMap<String, HashSet<String>> = HashMap::new();
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        
        // Initialize all steps
        for step in &manifest.steps {
            dependencies.insert(step.id.clone(), HashSet::new());
            dependents.insert(step.id.clone(), HashSet::new());
            in_degree.insert(step.id.clone(), 0);
        }
        
        // Build dependency graph by analyzing step inputs
        for step in &manifest.steps {
            let step_deps = self.extract_step_dependencies(step, &manifest.steps);
            dependencies.insert(step.id.clone(), step_deps.clone());
            
            // Update in-degree and dependents
            for dep in &step_deps {
                if let Some(dep_set) = dependents.get_mut(dep) {
                    dep_set.insert(step.id.clone());
                }
                *in_degree.get_mut(&step.id).unwrap() += 1;
            }
        }
        
        // Topological sort using Kahn's algorithm
        let mut queue: VecDeque<String> = VecDeque::new();
        let mut result: Vec<StepSpec> = Vec::new();
        
        // Find steps with no dependencies
        for (step_id, degree) in &in_degree {
            if *degree == 0 {
                queue.push_back(step_id.clone());
            }
        }
        
        while let Some(current_step_id) = queue.pop_front() {
            // Find the step definition
            let step = manifest.steps.iter()
                .find(|s| s.id == current_step_id)
                .ok_or_else(|| anyhow::anyhow!("Step {} not found", current_step_id))?;
            
            // Create StepSpec
            let step_spec = StepSpec {
                id: step.id.clone(),
                kind: self.determine_step_kind(step)?,
                ref_: step.uses.name.clone(),
                args: vec![],
                env: step.env.clone(),
                workdir: None,
                network: None,
                entry: None,
                mounts: vec![],
            };
            result.push(step_spec);
            
            // Update in-degree for dependents
            if let Some(dep_set) = dependents.get(&current_step_id) {
                for dependent in dep_set {
                    if let Some(degree) = in_degree.get_mut(dependent) {
                        *degree -= 1;
                        if *degree == 0 {
                            queue.push_back(dependent.clone());
                        }
                    }
                }
            }
        }
        
        // Check for circular dependencies
        if result.len() != manifest.steps.len() {
            return Err(anyhow::anyhow!("Circular dependency detected in step graph"));
        }
        
        println!("📋 Execution order: {:?}", result.iter().map(|s| &s.id).collect::<Vec<_>>());
        Ok(result)
    }

    fn extract_step_dependencies(&self, step: &crate::models::ShActionStep, all_steps: &[crate::models::ShActionStep]) -> HashSet<String> {
        let mut dependencies = HashSet::new();
        
        // Check step inputs for references to other steps
        for input in &step.inputs {
            if let Some(input_obj) = input.as_object() {
                if let Some(value) = input_obj.get("value") {
                    if let Some(value_str) = value.as_str() {
                        // Look for patterns like {{step_id.field}} or {{step_id.body[0].field}}
                        for other_step in all_steps {
                            if value_str.contains(&format!("{{{{{}.", other_step.id)) {
                                dependencies.insert(other_step.id.clone());
                            }
                        }
                    }
                }
            }
        }
        
        dependencies
    }

    fn determine_step_kind(&self, step: &crate::models::ShActionStep) -> Result<String> {
        // For now, determine based on the action reference
        // This could be enhanced to check the actual manifest of the referenced action
        if step.uses.name.contains("http-get-wasm") {
            Ok("wasm".to_string())
        } else if step.uses.name.contains("docker") {
            Ok("docker".to_string())
        } else {
            // Default to WASM for now
            Ok("wasm".to_string())
        }
    }

    async fn execute_plan(&self, plan: &ActionPlan, inputs: &HashMap<String, Value>) -> Result<Value> {
        // Prefetch all artifacts
        self.prefetch_all(plan).await?;
        
        // Execute with accumulated state
        let mut state = json!({});
        
        for step in &plan.steps {
            // Build step parameters based on the step's input requirements
            let step_params = self.build_step_parameters(step, &state, inputs)?;
            
            let patch = match step.kind.as_str() {
                "docker" => self.run_docker_step(step, plan.workdir.as_deref(), &step_params).await?,
                "wasm" => self.run_wasm_step(step, plan.workdir.as_deref(), &step_params).await?,
                other => bail!("unknown step.kind '{}'", other),
            };
            if !patch.is_null() {
                self.deep_merge(&mut state, patch);
            }
        }
        
        println!("=== final state ===\n{}", serde_json::to_string_pretty(&state)?);
        Ok(state)
    }

    fn build_step_parameters(&self, step: &StepSpec, state: &Value, inputs: &HashMap<String, Value>) -> Result<Value> {
        // For WASM modules, use simple key-value object format
        if step.kind == "wasm" {
            // Convert inputs to simple object format expected by WASM modules
            let mut input_obj = Map::new();
            
            for (key, value) in inputs {
                input_obj.insert(key.clone(), value.clone());
            }
            
            return Ok(Value::Object(input_obj));
        }
        
        // For Docker modules, use the complex object structure
        let mut params = Map::new();
        
        // Add environment variables as parameters
        for (k, v) in &step.env {
            params.insert(k.clone(), Value::String(v.clone()));
        }
        
        // Add state and inputs
        params.insert("state".to_string(), state.clone());
        params.insert("inputs".to_string(), Value::Object(inputs.clone().into_iter().collect()));
        
        Ok(Value::Object(params))
    }

    async fn prefetch_all(&self, plan: &ActionPlan) -> Result<()> {
        std::fs::create_dir_all(&self.cache_dir)?;
        for step in &plan.steps {
            match step.kind.as_str() {
                "docker" => { 
                    // For Docker, download and extract artifacts, then load the image
                    self.download_and_extract_artifacts(&step.ref_).await?;
                    self.load_docker_image(&step.ref_).await?;
                }
                "wasm" => { let _ = self.client.download_wasm(&step.ref_, &self.cache_dir).await; }
                _ => {}
            }
        }
        Ok(())
    }

    async fn load_docker_image(&self, action_ref: &str) -> Result<()> {
        if which("docker").is_err() {
            bail!("docker not found on PATH");
        }
        
        // Convert action_ref to cache directory path
        let action_cache_dir = self.cache_dir.join(action_ref.replace('/', "_").replace(":", "_"));
        
        // Look for Docker image files in the extracted artifacts
        let image_files = ["image.tar", "docker-image.tar", "image.tar.gz"];
        let mut image_path = None;
        
        for file_name in &image_files {
            let path = action_cache_dir.join(file_name);
            if path.exists() {
                image_path = Some(path);
                break;
            }
        }
        
        if let Some(image_path) = image_path {
            println!("Loading Docker image from: {}", image_path.display());
            let status = Command::new("docker")
                .arg("load")
                .arg("-i")
                .arg(&image_path)
                .status()
                .await?;
            
            if !status.success() {
                bail!("Failed to load Docker image from {}", image_path.display());
            }
        } else {
            bail!("No Docker image file found in artifacts for {}", action_ref);
        }
        
        Ok(())
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

        let tag_out = format!("[{}][stdout] ", step.id);
        let tag_err = format!("[{}][stderr] ", step.id);

        let tag_out_for_out = tag_out.clone();
        let tag_err_for_out = tag_err.clone();
        let quiet_logs = false;

        let pump_out = tokio::spawn(async move {
            while let Ok(Some(line)) = out_reader.next_line().await {
                if let Some(idx) = line.find(ST_MARKER) {
                    let json_part = &line[idx + ST_MARKER.len()..];
                    if let Ok(v) = serde_json::from_str::<Value>(json_part) {
                        let _ = tx.send(v);
                    } else if !quiet_logs {
                        eprintln!("{}[marker-parse-error] {}", tag_err_for_out, line);
                    }
                    if !quiet_logs {
                        println!("{}{}", tag_out_for_out, line);
                    }
                } else if !quiet_logs {
                    println!("{}{}", tag_out_for_out, line);
                }
            }
        });

        let tag_err_for_err = tag_err;
        let pump_err = tokio::spawn(async move {
            while let Ok(Some(line)) = err_reader.next_line().await {
                if !quiet_logs {
                    eprintln!("{}{}", tag_err_for_err, line);
                }
            }
        });

        let status = child.wait().await?;
        let _ = pump_out.await;
        let _ = pump_err.await;

        if !status.success() {
            bail!("step '{}' failed with {}", step.id, status);
        }

        // Merge all patches emitted by the action
        let mut patch = json!({});
        while let Ok(v) = rx.try_recv() {
            self.deep_merge(&mut patch, v);
        }
        Ok(patch)
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
        
        // Test wasmtime availability
        println!("🔍 Testing wasmtime availability...");
        let test_result = Command::new("wasmtime").arg("--version").output().await;
        match test_result {
            Ok(output) => {
                let version = String::from_utf8_lossy(&output.stdout);
                println!("✅ wasmtime version: {}", version.trim());
            },
            Err(e) => {
                println!("❌ wasmtime test failed: {}", e);
                return Err(anyhow::anyhow!("wasmtime is not working properly: {}", e));
            }
        }

        // ensure we have the .wasm component locally
        println!("📥 Downloading WASM module for: {}", step.ref_);
        let module_path = self.client.download_wasm(&step.ref_, &self.cache_dir).await?;
        println!("📁 WASM module path: {:?}", module_path);
        
        // Verify the WASM file exists and is readable
        if !module_path.exists() {
            return Err(anyhow::anyhow!("WASM file not found at: {:?}", module_path));
        }
        
        // Check if the file is readable
        if let Err(e) = std::fs::metadata(&module_path) {
            return Err(anyhow::anyhow!("WASM file not accessible at {:?}: {}", module_path, e));
        }
        
        println!("✅ WASM module verified and ready for execution");

        // build stdin payload - use the pre-built parameters
        let input_json = serde_json::to_string(state_in)?;
        println!("📤 Input JSON: {}", input_json);

        // Construct command
        let mut cmd = Command::new("wasmtime");
        cmd.arg("-S").arg("http");
        cmd.arg(&module_path);
        
        println!("🚀 Executing command: wasmtime -S http {:?}", module_path);

        // optional: pass extra args defined in step.args
        for a in &step.args { cmd.arg(a); }

        // pass env (tokens, etc.)
        for (k, v) in &step.env { cmd.env(k, v); }

        // working dir if absolute
        if let Some(wd) = step.workdir.as_deref().or(pipeline_workdir) {
            if wd.starts_with('/') { cmd.current_dir(wd); }
        }

        // spawn with piped stdio
        println!("🔄 Spawning wasmtime process...");
        let mut child = match cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(child) => {
                println!("✅ wasmtime process spawned successfully");
                child
            },
            Err(e) => {
                println!("❌ Failed to spawn wasmtime process: {}", e);
                return Err(anyhow::anyhow!("Failed to spawn wasmtime for step {}: {}", step.id, e));
            }
        };

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

        let tag_out = format!("[{}][stdout] ", step.id);
        let tag_err = format!("[{}][stderr] ", step.id);
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let quiet_logs = false;

        let tag_err_for_out = tag_err.clone();
        let tag_out_for_out = tag_out.clone();
        let pump_out = tokio::spawn(async move {
            while let Ok(Some(line)) = out_reader.next_line().await {
                if let Some(idx) = line.find(ST_MARKER) {
                    let json_part = &line[idx + ST_MARKER.len()..];
                    if let Ok(v) = serde_json::from_str::<Value>(json_part) {
                        let _ = tx.send(v);
                    } else if !quiet_logs {
                        eprintln!("{}[marker-parse-error] {}", tag_err_for_out, line);
                    }
                    if !quiet_logs { println!("{}{}", tag_out_for_out, line); }
                } else if !quiet_logs {
                    println!("{}{}", tag_out_for_out, line);
                }
            }
        });

        let tag_err_for_err = tag_err;
        let pump_err = tokio::spawn(async move {
            while let Ok(Some(line)) = err_reader.next_line().await {
                if !quiet_logs { eprintln!("{}{}", tag_err_for_err, line); }
            }
        });

        let status = child.wait().await?;
        let _ = pump_out.await;
        let _ = pump_err.await;

        if !status.success() {
            bail!("step '{}' failed with {}", step.id, status);
        }

        // merge all patches
        let mut patch = json!({});
        while let Ok(v) = rx.try_recv() { 
            self.deep_merge(&mut patch, v); 
        }
        Ok(patch)
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

    fn deep_merge(&self, a: &mut Value, b: Value) {
        match (a, b) {
            (Value::Object(ao), Value::Object(bo)) => {
                for (k, v) in bo { 
                    self.deep_merge(ao.entry(k).or_insert(Value::Null), v); 
                }
            }
            (a_slot, b_val) => { *a_slot = b_val; }
        }
    }
}
