// src/runners/local.rs
use anyhow::{Result, bail, Context};
use anyhow::{anyhow};         // you already import anyhow::Result/Context/bail; add anyhow()
use serde_json::{json, Value, Map};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, process::Command};
use std::{path::{Path, PathBuf}};
use std::fs;                                  // for reading local composite
use std::collections::{ HashSet, HashMap, VecDeque }; // you use VecDeque in topo
use which::which;
const ST_MARKER: &str = "::starthub:state::";
use tokio::sync::mpsc;

use super::{Runner, DeployCtx};
use crate::starthub_api::Client as HubClient;
use crate::runners::models::{ActionPlan, StepSpec, MountSpec};
// src/runners/local.rs (top)

use serde::{Deserialize};

#[derive(Deserialize, Debug, Clone)]
struct CompositeInput { name: String, #[serde(default)] r#type: String, #[serde(default)] description: String }

#[derive(Deserialize, Debug, Clone)]
struct CompositeOutput { name: String, #[serde(default)] r#type: String, #[serde(default)] description: String }

#[derive(Deserialize, Debug, Clone)]
struct CompStep {
    id: String,
    #[serde(default)] kind: String,            // "docker" (default) or "wasm"
    uses: String,
    #[serde(default)]
    with: HashMap<String, String>
}

#[derive(Deserialize, Debug, Clone)]
struct WireFrom {
    #[serde(default)] source: Option<String>, // "inputs"
    #[serde(default)] step: Option<String>,
    #[serde(default)] output: Option<String>,
    #[serde(default)] key: Option<String>,
    #[serde(default)] value: Option<Value>,   // literal
}

#[derive(Deserialize, Debug, Clone)]
struct WireTo { step: String, input: String }

#[derive(Deserialize, Debug, Clone)]
struct Wire { from: WireFrom, to: WireTo }

#[derive(Deserialize, Debug, Clone)]
struct CompositeSpec {
    name: String,
    version: String,
    #[serde(default)] description: String,
    inputs: Vec<CompositeInput>,
    outputs: Vec<CompositeOutput>,
    steps: Vec<CompStep>,
    #[serde(default)] wires: Vec<Wire>,
    #[serde(default)] export: Value, // optional; often { "project_id": { "from": {...} } }
}

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

async fn run_action_plan(client: &HubClient, plan: &ActionPlan, ctx: &DeployCtx) -> Result<()> {
    // Prefetch
    let cache_dir = dirs::cache_dir().unwrap_or(std::env::temp_dir()).join("starthub/oci");
    prefetch_all(client, plan, &cache_dir).await?;

    // Execute with accumulated state
    let mut state = serde_json::json!({});

    for s in &plan.steps {
        let mut step = s.clone();
        // merge CLI -e
        for (k,v) in &ctx.secrets {
            step.env.entry(k.clone()).or_insert(v.clone());
        }
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

    let mut q: std::collections::VecDeque<String> =
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
                let k = w.from.key.as_deref().ok_or_else(|| anyhow::anyhow!("wire 'from.inputs' missing 'key'"))?;
                inputs_map.get(k)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("missing composite input '{}'", k))?
            } else {
                bail!("unknown wire source '{}'", src);
            }
        } else if let Some(from_step) = w.from.step.as_ref() {
            let out = w.from.output.as_deref().ok_or_else(|| anyhow::anyhow!("wire 'from.step' missing 'output'"))?;
            step_outputs.get(from_step)
                .and_then(|m| m.get(out))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("step '{}' has no output '{}'", from_step, out))?
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

// helper: detect an OCI image ref (digest or tag)
fn looks_like_oci_image(s: &str) -> bool {
    s.contains("@sha256:") || (s.contains('/') && s.contains(':'))
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

// helper: create a single-step plan from an image ref
fn single_step_plan_from_image(image: &str, secrets: &[(String,String)]) -> ActionPlan {
    let mut env: HashMap<String,String> = HashMap::new();
    for (k,v) in secrets { env.insert(k.clone(), v.clone()); }

    ActionPlan {
        id: format!("image:{}", image),
        version: "1".into(),
        workdir: None,            // <-- was Some(".".into())
        steps: vec![StepSpec {
            id: "run".into(),
            kind: "docker".into(),
            ref_: image.to_string(),
            entry: None,
            args: vec![],                 // you can add default args here if needed
            env,
            mounts: vec![MountSpec {
                typ: "bind".into(),
                source: "./out".into(),
                target: "/out".into(),
                rw: true,
            }],
            timeout_ms: Some(300_000),
            network: Some("bridge".into()), // allow network by default for ad-hoc tests
            workdir: None,
        }],
    }
}


pub struct LocalRunner;

#[async_trait::async_trait]
impl Runner for LocalRunner {
    fn name(&self) -> &'static str { "local" }

    async fn ensure_auth(&self) -> Result<()> { Ok(()) }
    async fn prepare(&self, _ctx: &mut DeployCtx) -> Result<()> { Ok(()) }
    async fn put_files(&self, _ctx: &DeployCtx) -> Result<()> { Ok(()) }
    async fn set_secrets(&self, _ctx: &DeployCtx) -> Result<()> { Ok(()) }

    async fn dispatch(&self, ctx: &DeployCtx) -> Result<()> {
        // 0) Local composite? run it and return.
        if looks_like_local_composite(&ctx.action) {
            let comp = try_load_composite_spec(&ctx.action)?;
            let base  = std::env::var("STARTHUB_API").unwrap_or_else(|_| "https://api.starthub.so".to_string());
            let token = std::env::var("STARTHUB_TOKEN").ok();
            let client = HubClient::new(base, token);
            run_composite(&client, &comp, ctx).await?;
            println!("✓ Local execution complete");
            return Ok(());
        }

        // 1) Otherwise, try server-provided ActionPlan
        let base  = std::env::var("STARTHUB_API").unwrap_or_else(|_| "https://api.starthub.so".to_string());
        let token = std::env::var("STARTHUB_TOKEN").ok();
        let client = HubClient::new(base, token);

        match client.fetch_action_plan(&ctx.action, ctx.env.as_deref()).await {
            Ok(plan) => {
                run_action_plan(&client, &plan, ctx).await?;
            }
            Err(e_plan) => {
                if looks_like_oci_image(&ctx.action) {
                    tracing::warn!("backend plan fetch failed ({}); falling back to direct image: {}", e_plan, ctx.action);
                    let plan = single_step_plan_from_image(&ctx.action, &ctx.secrets);
                    run_action_plan(&client, &plan, ctx).await?;
                } else {
                    return Err(e_plan).context("fetching action plan / resolving composite")?;
                }
            }
        };

        println!("✓ Local execution complete");
        Ok(())
    }

}

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

async fn docker_pull(image: &str) -> Result<()> {
    if which("docker").is_err() {
        bail!("docker not found on PATH (required for docker steps)");
    }
    let _ = Command::new("docker").arg("pull").arg(image).status().await?;
    Ok(())
}

async fn run_composite(client: &HubClient, comp: &CompositeSpec, ctx: &DeployCtx) -> Result<()> {
    // 0) Build fast lookup tables
    let step_map: HashMap<_,_> = comp.steps.iter().map(|s| (s.id.clone(), s)).collect();

    // 1) Inputs map (from CLI -e)
    let mut inputs_map: HashMap<String,String> = HashMap::new();
    for inp in &comp.inputs {
        if let Some((_,v)) = ctx.secrets.iter().find(|(k,_)| k==&inp.name) {
            inputs_map.insert(inp.name.clone(), v.clone());
        }
    }

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
        let mut step_spec = StepSpec {
            id: sid.clone(),
            kind: step_kind.to_string(),   // was hardcoded to "docker"
            ref_: s.uses.clone(),
            entry: None,
            args: vec![],
            env: env.clone(),
            mounts: vec![],
            timeout_ms: Some(300_000),
            network: Some("bridge".into()),
            workdir: None,
        };

        // also merge ctx.secrets into env if you want them all available
        for (k,v) in &ctx.secrets {
            step_spec.env.entry(k.clone()).or_insert(v.clone());
        }

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


async fn run_docker_step_collect_state(
    step: &StepSpec,
    pipeline_workdir: Option<&str>,
    state_in: &Value,   // <— NEW
) -> Result<Value> {
    // ...
    // ...

    if which("docker").is_err() { bail!("docker not found on PATH"); }

    use serde_json::{json, Map};
    use tokio::io::AsyncWriteExt;

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
    // let quiet_logs = std::env::var("STARTHUB_QUIET_LOGS").is_ok();
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

fn wasmtime_major() -> Option<u32> {
    use std::process::Command;
    let out = Command::new("wasmtime").arg("--version").output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    // e.g. "wasmtime 36.0.0\n"
    let tok = s.split_whitespace().nth(1)?;
    tok.split('.').next()?.parse().ok()
}

async fn run_wasm_step(
    client: &HubClient,
    step: &crate::runners::models::StepSpec,
    pipeline_workdir: Option<&str>,
    cache_dir: &std::path::Path,
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

    // pick flags based on wasmtime version
    // let major = wasmtime_major().unwrap_or(0);

    // Construct command
    let mut cmd = Command::new("wasmtime");

    // if major >= 36 {
        // Preview2 HTTP shim — simplest form (no subcommand)
        // wasmtime -S http [--allow-http=...] <module.wasm>
    cmd.arg("-S").arg("http");
        // Optional outbound allowlist(s):
        // cmd.arg("--allow-http=api.digitalocean.com");
        // Finally the module path
    cmd.arg(&module_path);
    // } else if (14..=18).contains(&major) {
    //     // Legacy preview1 shim
    //     // wasmtime run --wasi-modules=experimental-wasi-http <module.wasm>
    //     cmd.arg("run")
    //     // .arg("--wasi-modules=experimental-wasi-http")
    //     .arg(&module_path);
    // } else {
    //     bail!(
    //         "Unsupported wasmtime major version {}. Use ≥36 (-S http) or 14–18 (legacy shim).",
    //         major
    //     );
    // }

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
        use tokio::io::AsyncWriteExt;
        stdin.write_all(input_json.as_bytes()).await?;
    }
    drop(child.stdin.take());

    // pump stdout/stderr and collect patches (unchanged from your version)
    use tokio::io::AsyncBufReadExt;
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
