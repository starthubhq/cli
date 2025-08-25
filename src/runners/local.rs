// src/runners/local.rs
use anyhow::{Result, bail, Context};
use serde_json::{json, Value, Map};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, process::Command};
use std::{path::{Path, PathBuf}};
use which::which;
const ST_MARKER: &str = "::starthub:state::";
use tokio::sync::mpsc;

use super::{Runner, DeployCtx};
use crate::starthub_api::Client as HubClient;
use crate::runners::models::{ActionPlan, StepSpec, MountSpec};
// src/runners/local.rs (top)
use std::collections::HashMap;

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
        let base  = std::env::var("STARTHUB_API").unwrap_or_else(|_| "https://api.starthub.so".to_string());
        let token = std::env::var("STARTHUB_TOKEN").ok();
        let client = HubClient::new(base, token);

        // Try backend plan first
        let plan = match client.fetch_action_plan(&ctx.action, ctx.env.as_deref()).await {
            Ok(p) => p,
            Err(e) if looks_like_oci_image(&ctx.action) => {
                tracing::warn!("backend plan fetch failed ({e}); falling back to direct image: {}", ctx.action);
                single_step_plan_from_image(&ctx.action, &ctx.secrets)
            }
            Err(e) => return Err(e).context("fetching action plan")?,
        };

        // Prefetch
        let cache_dir = dirs::cache_dir().unwrap_or(std::env::temp_dir()).join("starthub/oci");
        prefetch_all(&client, &plan, &cache_dir).await?;

        // Execute (merge CLI -e into each step)
        let mut state = serde_json::json!({}); // accumulated state

        for s in &plan.steps {
            let mut step = s.clone();

            // merge CLI -e into the step env (as you already do)
            for (k,v) in &ctx.secrets {
                step.env.entry(k.clone()).or_insert(v.clone());
            }

            // run and collect optional patch
            let patch = match step.kind.as_str() {
                "docker" => run_docker_step_collect_state(&step, plan.workdir.as_deref()).await?,
                "wasm"   => run_wasm_step(&client, &step, plan.workdir.as_deref(), &cache_dir).await.map(|_| Value::Null)?,
                other    => bail!("unknown step.kind '{}'", other),
            };

            // merge if we got a patch
            if !patch.is_null() {
                deep_merge(&mut state, patch);
            }
        }

        // (optional) show final state
        println!("=== final state ===\n{}", serde_json::to_string_pretty(&state)?);
        Ok(())
    }
}

// ---- Prefetch ----
async fn prefetch_all(client: &HubClient, plan: &ActionPlan, cache_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    for s in &plan.steps {
        match s.kind.as_str() {
            "docker" => { let _ = docker_pull(&s.ref_).await; }
            "wasm" => { let _path = client.download_wasm(&s.ref_, cache_dir).await?; }
            _ => {},
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

async fn run_docker_step_collect_state(step: &StepSpec, pipeline_workdir: Option<&str>) -> Result<Value> {
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
    let input = json!({ "state": {}, "params": Value::Object(params) }).to_string();
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
    let quiet_logs = true;

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


// ---- Execute wasm ----
async fn run_wasm_step(client: &HubClient, step: &StepSpec, pipeline_workdir: Option<&str>, cache_dir: &Path) -> Result<()> {
    #[cfg(not(feature="wasm"))]
    { bail!("wasm support not compiled (enable feature `wasm`)"); }

    #[cfg(feature="wasm")] {
        use wasmtime::{Engine, Module, Store, Linker};
        use wasmtime_wasi::preview2::{WasiCtxBuilder, Table, WasiView, WasiCtx, DirPerms, FilePerms};
        use wasmtime_wasi::sync::Dir;

        // Resolve to local module path (cache/local)
        let module_path = if step.ref_.starts_with("oci://") {
            // TODO: add OCI pull; for now error or map to https via registry gateway
            bail!("OCI-WASM not implemented yet in local runner");
        } else {
            client.download_wasm(&step.ref_, cache_dir).await?
        };

        let engine = Engine::default();
        let module = Module::from_file(&engine, &module_path)?;

        // WASI ctx
        let mut table = Table::new();
        let mut wasi_builder = WasiCtxBuilder::new();

        // args & env
        let mut args = vec![step.entry.clone().unwrap_or_else(|| "module".into())];
        args.extend(step.args.clone());
        let env_pairs: Vec<(&str,&str)> = step.env.iter().map(|(k,v)| (k.as_str(), v.as_str())).collect();
        wasi_builder = wasi_builder.args(&args)?.envs(&env_pairs)?;

        // mounts
        for m in &step.mounts {
            if m.typ != "bind" { continue; }
            let host = absolutize(&m.source, pipeline_workdir)?;
            let dir = Dir::open_ambient_dir(&host, wasmtime_wasi::sync::ambient_authority())?;
            let (dp, fp) = if m.rw { (DirPerms::all(), FilePerms::all()) } else { (DirPerms::READ, FilePerms::READ) };
            wasi_builder = wasi_builder.preopened_dir_with_permissions(dir, &m.target, dp, fp);
        }

        if let Some(wd) = step.workdir.as_ref().or(pipeline_workdir) {
            wasi_builder = wasi_builder.working_dir(wd);
        }

        let wasi = wasi_builder.inherit_stdin().inherit_stdout().inherit_stderr().build();
        struct Ctx { table: Table, wasi: WasiCtx }
        impl WasiView for Ctx { fn table(&mut self)->&mut Table{&mut self.table} fn ctx(&mut self)->&mut WasiCtx{&mut self.wasi} }
        let mut store = Store::new(&engine, Ctx{ table, wasi });
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::preview2::command::add_to_linker(&mut linker)?;

        let command = wasmtime_wasi::preview2::command::Command::instantiate(&mut store, &module, &linker)?;
        let (code, _res) = command.call(&mut store)?;
        if let Some(c) = code { if c != 0 { bail!("step '{}' exited with code {}", step.id, c); } }
        Ok(())
    }
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
