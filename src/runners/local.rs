// src/runners/local.rs
use anyhow::{Result, bail, Context, anyhow};
use serde_json::{json, Value, Map};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, process::Command};
use tokio::sync::mpsc;
use std::path::Path;
use std::collections::{HashSet, HashMap, VecDeque};
use which::which;

use super::{Runner, DeployCtx};
use crate::starthub_api::Client as HubClient;
use crate::models::ShManifest;
use crate::config::{STARTHUB_API_BASE, SUPABASE_ANON_KEY};
use crate::runners::models::{ActionPlan, StepSpec};

// ============================================================================
// CONSTANTS
// ============================================================================

#[allow(dead_code)]
const ST_MARKER: &str = "::starthub:state::";

// ============================================================================
// DATA STRUCTURES
// ============================================================================



// Use ShActionStep directly instead of duplicating
pub use crate::models::ShActionStep as ActionStep;

// Use the ShWire types from models.rs instead of duplicating
pub use crate::models::ShWire as Wire;



// ============================================================================
// MAIN RUNNER IMPLEMENTATION
// ============================================================================

pub struct LocalRunner;

#[async_trait::async_trait]
impl Runner for LocalRunner {
    fn name(&self) -> &'static str { "local" }

    async fn ensure_auth(&self) -> Result<()> { Ok(()) }
    async fn prepare(&self, _ctx: &mut DeployCtx) -> Result<()> { Ok(()) }
    async fn put_files(&self, _ctx: &DeployCtx) -> Result<()> { Ok(()) }


    async fn dispatch(&self, ctx: &DeployCtx) -> Result<()> {
        // 0) Local composite? run it and return.
        // if looks_like_local_composite(&ctx.action) {
        //     let comp = try_load_composite_spec(&ctx.action)?;
        //     let base  = std::env::var("STARTHUB_API").unwrap_or_else(|_| STARTHUB_API_BASE.to_string());
        //     let token = std::env::var("STARTHUB_TOKEN").ok();
        //     let client = HubClient::new(base, token);
        //     execute(&client, &comp, ctx).await?;
        //     println!("✓ Local execution complete");
        //     return Ok(());
        // }

        // 1) Try to fetch action details from the actions edge function
        let base  = std::env::var("STARTHUB_API").unwrap_or_else(|_| STARTHUB_API_BASE.to_string());
        // Always use the API key from config.rs for authentication
        let token = Some(SUPABASE_ANON_KEY.to_string());
        let client = HubClient::new(base, token);

        // Try to fetch action metadata first
        match client.fetch_action_metadata(&ctx.action).await {
            Ok(metadata) => {
                println!("✓ Fetched action details for {}", ctx.action);
                println!("Action: {}", metadata.name);
                println!("Version: {}", metadata.version_number);
                println!("Description: {}", metadata.description);
                
                if let Some(inputs) = &metadata.inputs {
                    println!("\nInputs:");
                    for input in inputs {
                        println!("  - {} ({}): {}", input.name, input.action_port_type, input.action_port_direction);
                    }
                }
                
                if let Some(outputs) = &metadata.outputs {
                    println!("\nOutputs:");
                    for output in outputs {
                        println!("  - {} ({}): {}", output.name, output.action_port_type, output.action_port_direction);
                    }
                }
                
                // Try to download the starthub.json file and recursively fetch all action manifests
                println!("\nAttempting to download composite action definition...");
                let mut visited = HashSet::new();
                match fetch_all_action_manifests(&client, &ctx.action, &mut visited).await {
                    Ok(manifests) => {
                        if manifests.is_empty() {
                            println!("No composite action definition found.");
                            println!("Note: This action cannot be executed locally as it requires the full implementation.");
                            println!("Use --runner github to deploy and run this action remotely.");
                            return Ok(());
                        }
                        
                        println!("✓ Successfully downloaded {} action manifest(s)", manifests.len());
                        
                        // Get the main manifest (first one)
                        let main_manifest = &manifests[0];
                        println!("Main action: {} (version: {})", main_manifest.name, main_manifest.version);
                        println!("Steps: {}", main_manifest.steps.len());
                        
                        // Convert to local types for topo_order
                        let local_steps: Vec<ActionStep> = main_manifest.steps.iter()
                            .map(convert_action_step)
                            .collect();
                        let local_wires: Vec<Wire> = main_manifest.wires.iter()
                            .map(convert_action_wire)
                            .collect();
                        
                        // Use topo_order to determine execution order
                        let order = topo_order(&local_steps, &local_wires)?;
                        println!("Execution order: {:?}", order);
                        
                        // For now, just show the plan. In the future, this could execute the composite action
                        println!("\nNote: Composite action execution is not yet implemented locally.");
                        println!("Use --runner github to deploy and run this action remotely.");
                        return Ok(());
                    }
                    Err(e) => {
                        println!("Failed to download composite action definition: {}", e);
                        println!("Note: This action cannot be executed locally as it requires the full implementation.");
                        println!("Use --runner github to deploy and run this action remotely.");
                        return Ok(());
                    }
                }
            }
            Err(e_metadata) => {
                tracing::debug!("Failed to fetch action metadata ({}), trying action plan", e_metadata);
                return Err(e_metadata).context("fetching action plan / resolving composite")?;
            }
        };
    }
}

// ============================================================================
// EXECUTION
// ============================================================================
#[allow(dead_code)]
async fn run_action_plan(client: &HubClient, plan: &ActionPlan, _ctx: &DeployCtx) -> Result<()> {
    // Prefetch
    let cache_dir = dirs::cache_dir().unwrap_or(std::env::temp_dir()).join("starthub/oci");
    prefetch_all(client, plan, &cache_dir).await?;

    // Execute with accumulated state
    let mut state = serde_json::json!({});

    for s in &plan.steps {
        let step = s.clone();
        // run (pass current state into stdin)
        let patch = match step.kind.as_str() {
            "docker" => run_docker_step_collect_state(&step, plan.workdir.as_deref(), &state).await?,
            "wasm"   => run_wasm_step(&client, &step, plan.workdir.as_deref(), &cache_dir, &state).await?,
            other    => bail!("unknown step.kind '{}'", other),
        };
        if !patch.is_null() { deep_merge(&mut state, patch); }
    }

    println!("=== final state ===\n{}", serde_json::to_string_pretty(&state)?);
    Ok(())
}

// ============================================================================
// STEP EXECUTION
// ============================================================================

#[allow(dead_code)]
async fn run_docker_step_collect_state(
    step: &StepSpec,
    pipeline_workdir: Option<&str>,
    state_in: &Value,
) -> Result<Value> {
    if which("docker").is_err() { bail!("docker not found on PATH"); }

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
            absolutize(&m.source, pipeline_workdir)?,
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

    // ---- feed stdin JSON (params from env for now) ----
    let mut params = Map::new();
    for (k,v) in &step.env { params.insert(k.clone(), Value::String(v.clone())); }
    let input = json!({ "state": state_in, "params": Value::Object(params) }).to_string();

    if let Some(stdin) = child.stdin.as_mut() { stdin.write_all(input.as_bytes()).await?; }
    drop(child.stdin.take());

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut out_reader = BufReader::new(stdout).lines();
    let mut err_reader = BufReader::new(stderr).lines();

    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();

    let tag_out = format!("[{}][stdout] ", step.id);
    let tag_err = format!("[{}][stderr] ", step.id);

    // clone per task to avoid move-after-move
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
                // optionally avoid echoing marker line in quiet mode
                if !quiet_logs {
                    println!("{}{}", tag_out_for_out, line);
                }
            } else if !quiet_logs {
                println!("{}{}", tag_out_for_out, line);
            }
        }
    });

    // move the original tag_err into the stderr task
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

    // Merge all patches emitted by the action (could be multiple)
    let mut patch = serde_json::json!({});
    while let Ok(v) = rx.try_recv() {
        deep_merge(&mut patch, v);
    }
    Ok(patch)
}

#[allow(dead_code)]
async fn run_wasm_step(
    client: &HubClient,
    step: &StepSpec,
    pipeline_workdir: Option<&str>,
    cache_dir: &Path,
    state_in: &Value,
) -> Result<Value> {
    if which::which("wasmtime").is_err() {
        bail!("`wasmtime` not found on PATH.");
    }

    // ensure we have the .wasm component locally
    let module_path = client.download_wasm(&step.ref_, cache_dir).await?;

    // build stdin payload
    let mut params = serde_json::Map::new();
    for (k, v) in &step.env { params.insert(k.clone(), Value::String(v.clone())); }
    let input_json = serde_json::json!({ "state": state_in, "params": Value::Object(params) }).to_string();

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

    // feed stdin JSON (stdin/out protocol)
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
    let mut patch = serde_json::json!({});
    while let Ok(v) = rx.try_recv() { deep_merge(&mut patch, v); }
    Ok(patch)
}

// ============================================================================
// UTILITY FUNCTIONS
// ============================================================================


#[allow(dead_code)]
fn absolutize(p: &str, base: Option<&str>) -> Result<String> {
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

// ============================================================================
// GRAPH ALGORITHMS
// ============================================================================

pub fn topo_order(steps: &[ActionStep], wires: &[Wire]) -> Result<Vec<String>> {
    let ids: HashSet<_> = steps.iter().map(|s| s.id.clone()).collect();
    let mut indeg: HashMap<String, usize> = steps.iter().map(|s| (s.id.clone(), 0)).collect();
    let mut adj: HashMap<String, Vec<String>> = steps.iter().map(|s| (s.id.clone(), vec![])).collect();

    for w in wires {
        if let (Some(from_step), _) = (w.from.step.as_ref(), w.to.step.as_str()) {
            if ids.contains(from_step) && ids.contains(&w.to.step) {
                adj.get_mut(from_step).unwrap().push(w.to.step.clone());
                *indeg.get_mut(&w.to.step).unwrap() += 1;
            }
        }
    }

    let mut q: VecDeque<String> =
        indeg.iter().filter(|(_, &d)| d == 0).map(|(k, _)| k.clone()).collect();
    let mut order = vec![];

    while let Some(u) = q.pop_front() {
        order.push(u.clone());
        if let Some(vs) = adj.get(&u) {
            for v in vs {
                let e = indeg.get_mut(v).unwrap();
                *e -= 1;
                if *e == 0 { q.push_back(v.clone()); }
            }
        }
    }

    if order.len() != steps.len() {
        bail!("cycle detected in composite steps wiring");
    }
    Ok(order)
}

// ============================================================================
// ENVIRONMENT AND WIRING
// ============================================================================

#[allow(dead_code)]
fn build_env_for_step(
    step_id: &str,
    step_with: &HashMap<String, serde_json::Value>,
    wires: &[Wire],
    inputs_map: &HashMap<String,String>,
    step_outputs: &HashMap<String, HashMap<String,String>>,
) -> Result<HashMap<String,String>> {
    let mut env: HashMap<String, String> = HashMap::new();
    // Convert step_with to string values
    for (k, v) in step_with {
        env.insert(k.clone(), v.as_str().unwrap_or("").to_string());
    }

    for w in wires.iter().filter(|w| w.to.step == step_id) {
        let key = w.to.input.clone();
        let val = if let Some(src) = w.from.source.as_deref() {
            if src == "inputs" {
                let k = w.from.key.as_deref().ok_or_else(|| anyhow!("wire 'from.inputs' missing 'key'"))?;
                inputs_map.get(k)
                    .cloned()
                    .ok_or_else(|| anyhow!("missing composite input '{}'", k))?
            } else {
                bail!("unknown wire source '{}'", src);
            }
        } else if let Some(from_step) = w.from.step.as_ref() {
            let out = w.from.output.as_deref().ok_or_else(|| anyhow!("wire 'from.step' missing 'output'"))?;
            step_outputs.get(from_step)
                .and_then(|m| m.get(out))
                .cloned()
                .ok_or_else(|| anyhow!("step '{}' has no output '{}'", from_step, out))?
        } else if let Some(v) = w.from.value.as_ref() {
            match v {
                Value::String(s) => s.clone(),
                _ => v.to_string(), // stringify non-strings
            }
        } else {
            bail!("wire 'from' must be one of inputs/step/value");
        };

        // last write wins if multiple wires set same input
        env.insert(key, val);
    }

    Ok(env)
}

// ============================================================================
// STATE MANAGEMENT
// ============================================================================

#[allow(dead_code)]
fn lookup_state_path(state: &Value, path: &str) -> Option<String> {
    let mut cur = state;
    for seg in path.split('.') {
        match cur {
            Value::Object(m) => { cur = m.get(seg)?; }
            Value::Array(a) => { let idx: usize = seg.parse().ok()?; cur = a.get(idx)?; }
            _ => return None,
        }
    }
    match cur {
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

#[allow(dead_code)]
fn derive_output_by_name(name: &str, state: &Value) -> Option<String> {
    // 1) exact key at root
    if let Some(v) = state.get(name) {
        return match v {
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        };
    }
    // 2) *_id => try <stem>.id
    if let Some(stem) = name.strip_suffix("_id") {
        if let Some(s) = lookup_state_path(state, &format!("{}.id", stem)) {
            return Some(s);
        }
    }
    None
}

#[allow(dead_code)]
fn collect_needed_outputs_for(step_id: &str, wires: &[Wire]) -> HashSet<String> {
    wires.iter()
        .filter(|w| w.from.step.as_deref()==Some(step_id))
        .filter_map(|w| w.from.output.clone())
        .collect()
}

#[allow(dead_code)]
fn extract_step_outputs(step_id: &str, state: &Value, wires: &[Wire]) -> HashMap<String,String> {
    let mut m = HashMap::new();
    for out_name in collect_needed_outputs_for(step_id, wires) {
        if let Some(val) = derive_output_by_name(&out_name, state) {
            m.insert(out_name, val);
        }
    }
    m
}

// Deep-merge: b overrides a
#[allow(dead_code)]
fn deep_merge(a: &mut Value, b: Value) {
    match (a, b) {
        (Value::Object(ao), Value::Object(bo)) => {
            for (k, v) in bo { deep_merge(ao.entry(k).or_insert(Value::Null), v); }
        }
        (a_slot, b_val) => { *a_slot = b_val; }
    }
}

// ============================================================================
// PLAN BUILDING
// ============================================================================

// helper: create a single-step plan from an image ref

// ============================================================================
// PREFETCHING
// ============================================================================

#[allow(dead_code)]
async fn prefetch_all(client: &HubClient, plan: &ActionPlan, cache_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    for s in &plan.steps {
        match s.kind.as_str() {
            "docker" => { let _ = docker_pull(&s.ref_).await; }
            "wasm"   => { let _ = client.download_wasm(&s.ref_, cache_dir).await?; }
            _ => {}
        }
    }
    Ok(())
}

#[allow(dead_code)]
async fn docker_pull(image: &str) -> Result<()> {
    if which("docker").is_err() {
        bail!("docker not found on PATH (required for docker steps)");
    }
    let _ = Command::new("docker").arg("pull").arg(image).status().await?;
    Ok(())
}

// ============================================================================
// ACTION METADATA CONVERSION
// ============================================================================


// ============================================================================
// RECURSIVE ACTION FETCHING
// ============================================================================

/// Convert ShActionStep to ActionStep (no conversion needed since we're using ShActionStep directly)
fn convert_action_step(step: &ActionStep) -> ActionStep {
    step.clone()
}

/// Convert Wire to Wire (no conversion needed since we're using ShWire directly)
fn convert_action_wire(wire: &Wire) -> Wire {
    wire.clone()
}

/// Recursively fetch all action manifests for a composite action
async fn fetch_all_action_manifests(
    client: &HubClient, 
    action_ref: &str,
    visited: &mut HashSet<String>
) -> Result<Vec<ShManifest>> {
    if visited.contains(action_ref) {
        return Ok(vec![]); // Already fetched this action
    }
    visited.insert(action_ref.to_string());
    
    // First, try to get the action metadata to find the storage URL
    let metadata = match client.fetch_action_metadata(action_ref).await {
        Ok(m) => m,
        Err(_) => {
            // If we can't get metadata, this might not be a composite action
            return Ok(vec![]);
        }
    };
    
    // Construct the storage URL for the starthub.json file
    let storage_url = format!(
        "https://api.starthub.so/storage/v1/object/public/git/{}/{}/starthub.json",
        action_ref.split('@').next().unwrap_or(""),
        metadata.commit_sha
    );
    
    // Download and parse the starthub.json file
    let manifest = client.download_starthub_json(&storage_url).await?;
    
    let mut all_manifests = vec![manifest.clone()];
    
    // Recursively fetch manifests for all steps
    for step in &manifest.steps {
        if let Ok(step_manifests) = Box::pin(fetch_all_action_manifests(client, &step.uses, visited)).await {
            all_manifests.extend(step_manifests);
        }
    }
    
    Ok(all_manifests)
}

