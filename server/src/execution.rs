use anyhow::{Result, bail, Context};
use serde_json::{Value, json, Map};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, process::Command};
use tokio::sync::mpsc;
use std::path::Path;
use std::collections::{HashSet, HashMap};
use which::which;

use crate::models::{ShManifest, HubClient, StepSpec, ActionPlan};

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
        
        std::fs::create_dir_all(&cache_dir).ok();
        
        Self {
            client: HubClient::new(base_url, token),
            cache_dir,
        }
    }

    pub async fn execute_action(&self, action_ref: &str, inputs: HashMap<String, Value>) -> Result<Value> {
        println!("🚀 Starting execution of action: {}", action_ref);
        
        // Fetch action metadata
        let metadata = self.client.fetch_action_metadata(action_ref).await?;
        println!("✓ Fetched action details: {}", metadata.name);
        
        // Try to download composite action definition
        let manifests = self.fetch_all_action_manifests(action_ref).await?;
        if manifests.is_empty() {
            return Err(anyhow::anyhow!("No composite action definition found for: {}", action_ref));
        }
        
        let main_manifest = &manifests[0];
        println!("✓ Main action: {} (version: {})", main_manifest.name, main_manifest.version);
        println!("✓ Steps: {}", main_manifest.steps.len());
        
        // Convert to execution plan
        let plan = self.build_execution_plan(main_manifest)?;
        
        // Execute the plan
        self.execute_plan(&plan, &inputs).await
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
        
        // Get action metadata
        let metadata = match self.client.fetch_action_metadata(action_ref).await {
            Ok(m) => m,
            Err(_) => return Ok(vec![]),
        };
        
        // Construct storage URL for starthub.json
        let storage_url = format!(
            "https://api.starthub.so/storage/v1/object/public/git/{}/{}/starthub.json",
            action_ref.split('@').next().unwrap_or(""),
            metadata.commit_sha
        );
        
        // Download and parse starthub.json
        let manifest = self.client.download_starthub_json(&storage_url).await?;
        let mut all_manifests = vec![manifest.clone()];
        
        // Recursively fetch manifests for all steps
        for step in &manifest.steps {
            if let Ok(step_manifests) = Box::pin(self.fetch_all_action_manifests_recursive(&step.uses.name, visited)).await {
                all_manifests.extend(step_manifests);
            }
        }
        
        Ok(all_manifests)
    }

    fn build_execution_plan(&self, manifest: &ShManifest) -> Result<ActionPlan> {
        let mut steps = Vec::new();
        
        for step in &manifest.steps {
            let step_spec = StepSpec {
                id: step.id.clone(),
                kind: "wasm".to_string(), // Default to WASM for now
                ref_: step.uses.name.clone(),
                args: vec![],
                env: step.env.clone(),
                workdir: None,
                network: None,
                entry: None,
                mounts: vec![],
            };
            steps.push(step_spec);
        }
        
        Ok(ActionPlan {
            steps,
            workdir: None,
        })
    }

    async fn execute_plan(&self, plan: &ActionPlan, _inputs: &HashMap<String, Value>) -> Result<Value> {
        // Prefetch all artifacts
        self.prefetch_all(plan).await?;
        
        // Execute with accumulated state
        let mut state = json!({});
        
        for step in &plan.steps {
            let patch = match step.kind.as_str() {
                "docker" => self.run_docker_step(step, plan.workdir.as_deref(), &state).await?,
                "wasm" => self.run_wasm_step(step, plan.workdir.as_deref(), &state).await?,
                other => bail!("unknown step.kind '{}'", other),
            };
            if !patch.is_null() {
                self.deep_merge(&mut state, patch);
            }
        }
        
        println!("=== final state ===\n{}", serde_json::to_string_pretty(&state)?);
        Ok(state)
    }

    async fn prefetch_all(&self, plan: &ActionPlan) -> Result<()> {
        std::fs::create_dir_all(&self.cache_dir)?;
        for step in &plan.steps {
            match step.kind.as_str() {
                "docker" => { let _ = self.docker_pull(&step.ref_).await; }
                "wasm" => { let _ = self.client.download_wasm(&step.ref_, &self.cache_dir).await; }
                _ => {}
            }
        }
        Ok(())
    }

    async fn docker_pull(&self, image: &str) -> Result<()> {
        if which("docker").is_err() {
            bail!("docker not found on PATH");
        }
        let _ = Command::new("docker").arg("pull").arg(image).status().await?;
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

        // image + args
        cmd.arg(&step.ref_);
        for a in &step.args { cmd.arg(a); }

        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning docker for step {}", step.id))?;

        // feed stdin JSON
        let mut params = Map::new();
        for (k,v) in &step.env { params.insert(k.clone(), Value::String(v.clone())); }
        let input = json!({ "state": state_in, "params": Value::Object(params) }).to_string();

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

        // ensure we have the .wasm component locally
        let module_path = self.client.download_wasm(&step.ref_, &self.cache_dir).await?;

        // build stdin payload
        let mut params = Map::new();
        for (k, v) in &step.env { params.insert(k.clone(), Value::String(v.clone())); }
        let input_json = json!({ "state": state_in, "params": Value::Object(params) }).to_string();

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
            .with_context(|| format!("spawning wasmtime for step {}", step.id))?;

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
