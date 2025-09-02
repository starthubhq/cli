// src/runners/local.rs
use anyhow::{Result, bail, Context, anyhow};
use serde::{Deserialize};
use serde_json::{json, Value, Map};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, process::Command};
use tokio::sync::mpsc;
use std::path::Path;
use std::fs;
use std::collections::{HashSet, HashMap, VecDeque};
use which::which;

use super::{Runner, DeployCtx};
use crate::starthub_api::{Client as HubClient, ActionMetadata};
use crate::config::{STARTHUB_API_BASE, STARTHUB_API_KEY};
use crate::runners::models::{ActionPlan, StepSpec, MountSpec};

// ============================================================================
// CONSTANTS
// ============================================================================

const ST_MARKER: &str = "::starthub:state::";

// ============================================================================
// DATA STRUCTURES
// ============================================================================

#[derive(Deserialize, Debug, Clone)]
struct CompositeInput { 
    #[allow(dead_code)]
    name: String, 
    #[serde(default)] 
    #[allow(dead_code)]
    r#type: String, 
    #[serde(default)] 
    #[allow(dead_code)]
    description: String 
}

#[derive(Deserialize, Debug, Clone)]
struct CompositeOutput { 
    #[allow(dead_code)]
    name: String, 
    #[serde(default)] 
    #[allow(dead_code)]
    r#type: String, 
    #[serde(default)] 
    #[allow(dead_code)]
    description: String 
}

#[derive(Deserialize, Debug, Clone)]
struct CompStep {
    id: String,
    #[serde(default)] 
    kind: String,            // "docker" (default) or "wasm"
    uses: String,
    #[serde(default)]
    with: HashMap<String, String>
}

#[derive(Deserialize, Debug, Clone)]
struct WireFrom {
    #[serde(default)] 
    source: Option<String>, // "inputs"
    #[serde(default)] 
    step: Option<String>,
    #[serde(default)] 
    output: Option<String>,
    #[serde(default)] 
    key: Option<String>,
    #[serde(default)] 
    value: Option<Value>,   // literal
}

#[derive(Deserialize, Debug, Clone)]
struct WireTo { 
    step: String, 
    input: String 
}

#[derive(Deserialize, Debug, Clone)]
struct Wire { 
    from: WireFrom, 
    to: WireTo 
}

#[derive(Deserialize, Debug, Clone)]
struct CompositeSpec {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    version: String,
    #[serde(default)] 
    #[allow(dead_code)]
    description: String,
    inputs: Vec<CompositeInput>,
    #[allow(dead_code)]
    outputs: Vec<CompositeOutput>,
    steps: Vec<CompStep>,
    #[serde(default)] 
    wires: Vec<Wire>,
    #[serde(default)] 
    export: Value, // optional; often { "project_id": { "from": {...} } }
}

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
        if looks_like_local_composite(&ctx.action) {
            let comp = try_load_composite_spec(&ctx.action)?;
            let base  = std::env::var("STARTHUB_API").unwrap_or_else(|_| STARTHUB_API_BASE.to_string());
            let token = std::env::var("STARTHUB_TOKEN").ok();
            let client = HubClient::new(base, token);
            run_composite(&client, &comp, ctx).await?;
            println!("✓ Local execution complete");
            return Ok(());
        }

        // 1) Try to fetch action details from the actions edge function
        let base  = std::env::var("STARTHUB_API").unwrap_or_else(|_| STARTHUB_API_BASE.to_string());
        // Always use the API key from config.rs for authentication
        let token = Some(STARTHUB_API_KEY.to_string());
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
                
                println!("\nNote: This action cannot be executed locally as it requires the full implementation.");
                println!("Use --runner github to deploy and run this action remotely.");
                return Ok(());
            }
            Err(e_metadata) => {
                tracing::debug!("Failed to fetch action metadata ({}), trying action plan", e_metadata);
                return Err(e_metadata).context("fetching action plan / resolving composite")?;
            }
        };
    }
}

// ============================================================================
// COMPOSITE EXECUTION
// ============================================================================

async fn run_composite(client: &HubClient, comp: &CompositeSpec, _ctx: &DeployCtx) -> Result<()> {
    // 0) Build fast lookup tables
    let step_map: HashMap<_,_> = comp.steps.iter().map(|s| (s.id.clone(), s)).collect();

    // 1) Inputs map (empty since no secrets are supported)
    let inputs_map: HashMap<String,String> = HashMap::new();

    // 2) Prefetch (do both docker & wasm)
    for s in &comp.steps {
        match s.kind.as_str() {
            "docker" => { let _ = docker_pull(&s.uses).await; }
            "wasm"   => { let _ = client.download_wasm(&s.uses, &std::env::temp_dir().join("starthub/oci")).await?; }
            _ => {}
        }
    }

    // 3) Run in topo order
    let order = topo_order(&comp.steps, &comp.wires)?;
    let mut state = json!({});
    let mut step_outputs: HashMap<String, HashMap<String,String>> = HashMap::new();

    for sid in order {
        let s = step_map.get(&sid).unwrap();
        let step_kind = if s.kind.is_empty() { "docker" } else { &s.kind };

        // 3a) Resolve env for this step from wires + with
        let env = build_env_for_step(&sid, &s.with, &comp.wires, &inputs_map, &step_outputs)?;

        // 3b) Build a transient StepSpec to reuse docker executor
        let step_spec = StepSpec {
            id: sid.clone(),
            kind: step_kind.to_string(),
            ref_: s.uses.clone(),
            entry: None,
            args: vec![],
            env: env.clone(),
            mounts: vec![],
            timeout_ms: Some(300_000),
            network: Some("bridge".into()),
            workdir: None,
        };

        // 3c) Run it (pass current state)
        let patch = match step_kind {
            "docker" => run_docker_step_collect_state(&step_spec, None, &state).await?,
            "wasm"   => run_wasm_step(client, &step_spec, None, &std::env::temp_dir().join("starthub/oci"), &state).await?,
            other    => bail!("unknown step.kind '{}'", other),
        };

        // 3d) Merge state
        if !patch.is_null() { deep_merge(&mut state, patch); }

        // 3e) Compute step outputs (only those referenced by downstream wires)
        let outs = extract_step_outputs(&sid, &state, &comp.wires);
        step_outputs.insert(sid.clone(), outs);
    }

    // 4) Composite export (optional)
    if !comp.export.is_null() {
        // Very small evaluator that supports { "from": { step/key/value } }
        if let Some(project_id_from) = comp.export.get("project_id") {
            if let Some(from) = project_id_from.get("from") {
                let val = if let Some(s) = from.get("step").and_then(|x| x.as_str()) {
                    let out = from.get("output").and_then(|x| x.as_str()).unwrap_or("");
                    step_outputs.get(s).and_then(|m| m.get(out)).cloned().unwrap_or_default()
                } else if let Some(src) = from.get("source").and_then(|x| x.as_str()) {
                    if src == "inputs" {
                        let key = from.get("key").and_then(|x| x.as_str()).unwrap_or("");
                        inputs_map.get(key).cloned().unwrap_or_default()
                    } else { String::new() }
                } else if let Some(v) = from.get("value") {
                    if let Some(s) = v.as_str() { s.to_string() } else { v.to_string() }
                } else { String::new() };

                println!("project_id={}", val);
            }
        }
    }

    // Also print final state for debugging
    println!("=== final state ===\n{}", serde_json::to_string_pretty(&state)?);

    Ok(())
}

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

fn looks_like_local_composite(s: &str) -> bool {
    let t = s.trim();
    t.starts_with('{') || t.ends_with(".json") || t.starts_with("file://") || Path::new(t).exists()
}

fn try_load_composite_spec(action: &str) -> Result<CompositeSpec> {
    let mut s = action.trim();
    if let Some(rest) = s.strip_prefix("file://") { s = rest; }

    if s.starts_with('{') {
        return Ok(serde_json::from_str::<CompositeSpec>(s).context("parsing inline composite json")?);
    }
    if s.ends_with(".json") || Path::new(s).exists() {
        let txt = fs::read_to_string(s).with_context(|| format!("reading composite file '{}'", s))?;
        return Ok(serde_json::from_str::<CompositeSpec>(&txt).context("parsing composite json")?);
    }
    Err(anyhow!("not a composite spec reference: {}", action))
}

#[allow(dead_code)]
fn looks_like_oci_image(s: &str) -> bool {
    s.contains("@sha256:") || s.contains(':')
}

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

fn topo_order(steps: &[CompStep], wires: &[Wire]) -> Result<Vec<String>> {
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

fn build_env_for_step(
    step_id: &str,
    step_with: &HashMap<String,String>,
    wires: &[Wire],
    inputs_map: &HashMap<String,String>,
    step_outputs: &HashMap<String, HashMap<String,String>>,
) -> Result<HashMap<String,String>> {
    let mut env = step_with.clone(); // defaults/overrides from step.with

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

fn collect_needed_outputs_for(step_id: &str, wires: &[Wire]) -> HashSet<String> {
    wires.iter()
        .filter(|w| w.from.step.as_deref()==Some(step_id))
        .filter_map(|w| w.from.output.clone())
        .collect()
}

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
#[allow(dead_code)]
fn single_step_plan_from_image(image: &str, secrets: &[(String,String)]) -> ActionPlan {
    let mut env: HashMap<String,String> = HashMap::new();
    for (k,v) in secrets { env.insert(k.clone(), v.clone()); }

    ActionPlan {
        id: format!("image:{}", image),
        version: "1".into(),
        workdir: None,
        steps: vec![StepSpec {
            id: "run".into(),
            kind: "docker".into(),
            ref_: image.to_string(),
            entry: None,
            args: vec![],
            env,
            mounts: vec![MountSpec {
                typ: "bind".into(),
                source: "./out".into(),
                target: "/out".into(),
                rw: true,
            }],
            timeout_ms: Some(300_000),
            network: Some("bridge".into()),
            workdir: None,
        }],
    }
}

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

/// Convert ActionMetadata from the API to a CompositeSpec for local execution
fn convert_action_metadata_to_composite(metadata: &ActionMetadata) -> Result<CompositeSpec> {
    // Since the current API doesn't provide steps/wires/export, create a simple single-step composite
    // that can be executed locally
    let comp_steps = vec![CompStep {
        id: "run".to_string(),
        kind: "docker".to_string(),
        uses: "alpine:latest".to_string(), // Default to alpine for now
        with: std::collections::HashMap::new(),
    }];

    let inputs = metadata.inputs.as_ref()
        .map(|inputs| inputs.iter().map(|input| CompositeInput {
            name: input.name.clone(),
            r#type: input.action_port_type.clone(),
            description: format!("Input: {}", input.action_port_direction),
        }).collect())
        .unwrap_or_default();

    let outputs = metadata.outputs.as_ref()
        .map(|outputs| outputs.iter().map(|output| CompositeOutput {
            name: output.name.clone(),
            r#type: output.action_port_type.clone(),
            description: format!("Output: {}", output.action_port_direction),
        }).collect())
        .unwrap_or_default();

    Ok(CompositeSpec {
        name: metadata.name.clone(),
        version: metadata.version_number.clone(),
        description: metadata.description.clone(),
        inputs,
        outputs,
        steps: comp_steps,
        wires: vec![], // No wires defined in current API
        export: serde_json::json!({}), // No export defined in current API
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;
    use std::fs;
    use std::collections::HashMap;

    // ============================================================================
    // TEST HELPERS
    // ============================================================================

    fn create_test_composite_spec() -> CompositeSpec {
        CompositeSpec {
            name: "test-composite".to_string(),
            version: "1.0.0".to_string(),
            description: "Test composite for unit testing".to_string(),
            inputs: vec![
                CompositeInput {
                    name: "test_input".to_string(),
                    r#type: "string".to_string(),
                    description: "Test input".to_string(),
                }
            ],
            outputs: vec![
                CompositeOutput {
                    name: "test_output".to_string(),
                    r#type: "string".to_string(),
                    description: "Test output".to_string(),
                }
            ],
            steps: vec![
                CompStep {
                    id: "step1".to_string(),
                    kind: "docker".to_string(),
                    uses: "alpine:latest".to_string(),
                    with: HashMap::new(),
                }
            ],
            wires: vec![],
            export: json!({}),
        }
    }

    fn create_test_deploy_ctx() -> DeployCtx {
        DeployCtx {
            action: "test-action".to_string(),
            owner: None,
            repo: None,
        }
    }

    // ============================================================================
    // UTILITY FUNCTION TESTS
    // ============================================================================

    #[test]
    fn test_looks_like_local_composite() {
        // Test JSON string
        assert!(looks_like_local_composite("{ \"name\": \"test\" }"));
        
        // Test JSON file path
        assert!(looks_like_local_composite("test.json"));
        
        // Test file:// protocol
        assert!(looks_like_local_composite("file://test.json"));
        
        // Test regular string (should be false)
        assert!(!looks_like_local_composite("just-a-string"));
    }

    #[test]
    fn test_looks_like_oci_image() {
        // Test with digest
        assert!(looks_like_oci_image("alpine@sha256:abc123"));
        
        // Test with tag
        assert!(looks_like_oci_image("alpine:latest"));
        
        // Test with registry
        assert!(looks_like_oci_image("docker.io/alpine:latest"));
        
        // Test regular string (should be false)
        assert!(!looks_like_oci_image("just-a-string"));
    }

    #[test]
    fn test_absolutize() {
        // Test absolute path (use a path that actually exists)
        let abs_path = absolutize("/tmp", None).unwrap();
        assert!(abs_path.starts_with('/'));
        
        // Test relative path with base (use a known absolute path)
        let rel_path = absolutize("tmp", Some("/")).unwrap();
        assert!(rel_path.contains("tmp"));
    }

    // ============================================================================
    // GRAPH ALGORITHM TESTS
    // ============================================================================

    #[test]
    fn test_topo_order_simple() {
        let steps = vec![
            CompStep {
                id: "step1".to_string(),
                kind: "docker".to_string(),
                uses: "alpine:latest".to_string(),
                with: HashMap::new(),
            },
            CompStep {
                id: "step2".to_string(),
                kind: "docker".to_string(),
                uses: "alpine:latest".to_string(),
                with: HashMap::new(),
            }
        ];
        
        let wires = vec![
            Wire {
                from: WireFrom {
                    source: None,
                    step: Some("step1".to_string()),
                    output: Some("output".to_string()),
                    key: None,
                    value: None,
                },
                to: WireTo {
                    step: "step2".to_string(),
                    input: "input".to_string(),
                }
            }
        ];
        
        let order = topo_order(&steps, &wires).unwrap();
        assert_eq!(order, vec!["step1", "step2"]);
    }

    #[test]
    fn test_topo_order_cycle_detection() {
        let steps = vec![
            CompStep {
                id: "step1".to_string(),
                kind: "docker".to_string(),
                uses: "alpine:latest".to_string(),
                with: HashMap::new(),
            },
            CompStep {
                id: "step2".to_string(),
                kind: "docker".to_string(),
                uses: "alpine:latest".to_string(),
                with: HashMap::new(),
            }
        ];
        
        let wires = vec![
            Wire {
                from: WireFrom {
                    source: None,
                    step: Some("step1".to_string()),
                    output: Some("output".to_string()),
                    key: None,
                    value: None,
                },
                to: WireTo {
                    step: "step2".to_string(),
                    input: "input".to_string(),
                }
            },
            Wire {
                from: WireFrom {
                    source: None,
                    step: Some("step2".to_string()),
                    output: Some("output".to_string()),
                    key: None,
                    value: None,
                },
                to: WireTo {
                    step: "step1".to_string(),
                    input: "input".to_string(),
                }
            }
        ];
        
        let result = topo_order(&steps, &wires);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cycle detected"));
    }

    // ============================================================================
    // STATE MANAGEMENT TESTS
    // ============================================================================

    #[test]
    fn test_lookup_state_path() {
        let state = json!({
            "user": {
                "name": "test",
                "profile": {
                    "age": 25
                }
            },
            "items": ["item1", "item2"]
        });
        
        // Test object path
        assert_eq!(lookup_state_path(&state, "user.name"), Some("test".to_string()));
        assert_eq!(lookup_state_path(&state, "user.profile.age"), Some("25".to_string()));
        
        // Test array path
        assert_eq!(lookup_state_path(&state, "items.0"), Some("item1".to_string()));
        assert_eq!(lookup_state_path(&state, "items.1"), Some("item2".to_string()));
        
        // Test non-existent path
        assert_eq!(lookup_state_path(&state, "user.nonexistent"), None);
    }

    #[test]
    fn test_derive_output_by_name() {
        let state = json!({
            "project_id": "123",
            "user": {
                "id": "456"
            }
        });
        
        // Test exact match
        assert_eq!(derive_output_by_name("project_id", &state), Some("123".to_string()));
        
        // Test _id suffix
        assert_eq!(derive_output_by_name("user_id", &state), Some("456".to_string()));
        
        // Test non-existent
        assert_eq!(derive_output_by_name("nonexistent", &state), None);
    }

    #[test]
    fn test_deep_merge() {
        let mut a = json!({
            "user": {
                "name": "old",
                "age": 25
            },
            "settings": {
                "theme": "dark"
            }
        });
        
        let b = json!({
            "user": {
                "name": "new"
            },
            "settings": {
                "theme": "light",
                "language": "en"
            }
        });
        
        deep_merge(&mut a, b);
        
        let expected = json!({
            "user": {
                "name": "new",
                "age": 25
            },
            "settings": {
                "theme": "light",
                "language": "en"
            }
        });
        
        assert_eq!(a, expected);
    }

    // ============================================================================
    // ENVIRONMENT AND WIRING TESTS
    // ============================================================================

    #[test]
    fn test_build_env_for_step() {
        let step_with = {
            let mut map = HashMap::new();
            map.insert("default_key".to_string(), "default_value".to_string());
            map
        };
        
        let wires = vec![
            Wire {
                from: WireFrom {
                    source: Some("inputs".to_string()),
                    step: None,
                    output: None,
                    key: Some("test_input".to_string()),
                    value: None,
                },
                to: WireTo {
                    step: "step1".to_string(),
                    input: "input1".to_string(),
                }
            },
            Wire {
                from: WireFrom {
                    source: None,
                    step: Some("step0".to_string()),
                    output: Some("output0".to_string()),
                    key: None,
                    value: None,
                },
                to: WireTo {
                    step: "step1".to_string(),
                    input: "input2".to_string(),
                }
            },
            Wire {
                from: WireFrom {
                    source: None,
                    step: None,
                    output: None,
                    key: None,
                    value: Some(json!("literal_value")),
                },
                to: WireTo {
                    step: "step1".to_string(),
                    input: "input3".to_string(),
                }
            }
        ];
        
        let inputs_map = {
            let mut map = HashMap::new();
            map.insert("test_input".to_string(), "test_value".to_string());
            map
        };
        
        let step_outputs = {
            let mut map = HashMap::new();
            let mut outputs = HashMap::new();
            outputs.insert("output0".to_string(), "step0_output".to_string());
            map.insert("step0".to_string(), outputs);
            map
        };
        
        let env = build_env_for_step("step1", &step_with, &wires, &inputs_map, &step_outputs).unwrap();
        
        assert_eq!(env.get("default_key"), Some(&"default_value".to_string()));
        assert_eq!(env.get("input1"), Some(&"test_value".to_string()));
        assert_eq!(env.get("input2"), Some(&"step0_output".to_string()));
        assert_eq!(env.get("input3"), Some(&"literal_value".to_string()));
    }

    #[test]
    fn test_collect_needed_outputs_for() {
        let wires = vec![
            Wire {
                from: WireFrom {
                    source: None,
                    step: Some("step1".to_string()),
                    output: Some("output1".to_string()),
                    key: None,
                    value: None,
                },
                to: WireTo {
                    step: "step2".to_string(),
                    input: "input1".to_string(),
                }
            },
            Wire {
                from: WireFrom {
                    source: None,
                    step: Some("step1".to_string()),
                    output: Some("output2".to_string()),
                    key: None,
                    value: None,
                },
                to: WireTo {
                    step: "step3".to_string(),
                    input: "input2".to_string(),
                }
            }
        ];
        
        let outputs = collect_needed_outputs_for("step1", &wires);
        assert_eq!(outputs.len(), 2);
        assert!(outputs.contains("output1"));
        assert!(outputs.contains("output2"));
    }

    // ============================================================================
    // PLAN BUILDING TESTS
    // ============================================================================

    #[test]
    fn test_single_step_plan_from_image() {
        let secrets = vec![
            ("key1".to_string(), "value1".to_string()),
            ("key2".to_string(), "value2".to_string()),
        ];
        
        let plan = single_step_plan_from_image("alpine:latest", &secrets);
        
        assert_eq!(plan.id, "image:alpine:latest");
        assert_eq!(plan.version, "1");
        assert_eq!(plan.steps.len(), 1);
        
        let step = &plan.steps[0];
        assert_eq!(step.id, "run");
        assert_eq!(step.kind, "docker");
        assert_eq!(step.ref_, "alpine:latest");
        assert_eq!(step.env.get("key1"), Some(&"value1".to_string()));
        assert_eq!(step.env.get("key2"), Some(&"value2".to_string()));
        assert_eq!(step.mounts.len(), 1);
        assert_eq!(step.mounts[0].typ, "bind");
        assert_eq!(step.mounts[0].source, "./out");
        assert_eq!(step.mounts[0].target, "/out");
        assert!(step.mounts[0].rw);
    }

    // ============================================================================
    // COMPOSITE SPEC PARSING TESTS
    // ============================================================================

    #[test]
    fn test_try_load_composite_spec_inline() {
        let json_str = r#"{
            "name": "test",
            "version": "1.0.0",
            "inputs": [],
            "outputs": [],
            "steps": [],
            "wires": []
        }"#;
        
        let spec = try_load_composite_spec(json_str).unwrap();
        assert_eq!(spec.name, "test");
        assert_eq!(spec.version, "1.0.0");
    }

    #[test]
    fn test_try_load_composite_spec_file() {
        let temp_dir = tempdir().unwrap();
        let file_path = temp_dir.path().join("test.json");
        
        let json_content = r#"{
            "name": "test",
            "version": "1.0.0",
            "inputs": [],
            "outputs": [],
            "steps": [],
            "wires": []
        }"#;
        
        fs::write(&file_path, json_content).unwrap();
        
        let spec = try_load_composite_spec(file_path.to_str().unwrap()).unwrap();
        assert_eq!(spec.name, "test");
        assert_eq!(spec.version, "1.0.0");
    }

    #[test]
    fn test_try_load_composite_spec_invalid() {
        let result = try_load_composite_spec("not-a-valid-reference");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not a composite spec reference"));
    }

    // ============================================================================
    // INTEGRATION TESTS
    // ============================================================================

    #[tokio::test]
    async fn test_local_runner_name() {
        let runner = LocalRunner;
        assert_eq!(runner.name(), "local");
    }

    #[tokio::test]
    async fn test_local_runner_auth_and_prepare() {
        let runner = LocalRunner;
        let mut ctx = create_test_deploy_ctx();
        
        // These should succeed without any external dependencies
        assert!(runner.ensure_auth().await.is_ok());
        assert!(runner.prepare(&mut ctx).await.is_ok());
        assert!(runner.put_files(&ctx).await.is_ok());

    }

    // ============================================================================
    // ERROR HANDLING TESTS
    // ============================================================================

    #[test]
    fn test_build_env_for_step_missing_key() {
        let wires = vec![
            Wire {
                from: WireFrom {
                    source: Some("inputs".to_string()),
                    step: None,
                    output: None,
                    key: None, // Missing key
                    value: None,
                },
                to: WireTo {
                    step: "step1".to_string(),
                    input: "input1".to_string(),
                }
            }
        ];
        
        let result = build_env_for_step("step1", &HashMap::new(), &wires, &HashMap::new(), &HashMap::new());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing 'key'"));
    }

    #[test]
    fn test_build_env_for_step_missing_output() {
        let wires = vec![
            Wire {
                from: WireFrom {
                    source: None,
                    step: Some("step0".to_string()),
                    output: None, // Missing output
                    key: None,
                    value: None,
                },
                to: WireTo {
                    step: "step1".to_string(),
                    input: "input1".to_string(),
                }
            }
        ];
        
        let result = build_env_for_step("step1", &HashMap::new(), &wires, &HashMap::new(), &HashMap::new());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing 'output'"));
    }

    #[test]
    fn test_build_env_for_step_unknown_source() {
        let wires = vec![
            Wire {
                from: WireFrom {
                    source: Some("unknown".to_string()), // Unknown source
                    step: None,
                    output: None,
                    key: Some("key".to_string()),
                    value: None,
                },
                to: WireTo {
                    step: "step1".to_string(),
                    input: "input1".to_string(),
                }
            }
        ];
        
        let result = build_env_for_step("step1", &HashMap::new(), &wires, &HashMap::new(), &HashMap::new());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown wire source"));
    }

    #[test]
    fn test_build_env_for_step_invalid_wire() {
        let wires = vec![
            Wire {
                from: WireFrom {
                    source: None,
                    step: None,
                    output: None,
                    key: None,
                    value: None, // No valid source
                },
                to: WireTo {
                    step: "step1".to_string(),
                    input: "input1".to_string(),
                }
            }
        ];
        
        let result = build_env_for_step("step1", &HashMap::new(), &wires, &HashMap::new(), &HashMap::new());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must be one of inputs/step/value"));
    }
}
