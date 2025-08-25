// src/runners/local.rs
use anyhow::{Result, bail, Context};
use tokio::{io::{AsyncBufReadExt, BufReader}, process::Command};
use std::{path::{Path, PathBuf}};
use which::which;

use super::{Runner, DeployCtx};
use crate::starthub_api::Client as HubClient;
use crate::runners::models::{ActionPlan, StepSpec, MountSpec};
// src/runners/local.rs (top)
use std::collections::HashMap;

// helper: detect an OCI image ref (digest or tag)
fn looks_like_oci_image(s: &str) -> bool {
    s.contains("@sha256:") || (s.contains('/') && s.contains(':'))
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
        for s in &plan.steps {
            let mut step = s.clone();
            for (k,v) in &ctx.secrets {
                step.env.entry(k.clone()).or_insert(v.clone());
            }
            match step.kind.as_str() {
                "docker" => run_docker_step(&step, plan.workdir.as_deref()).await?,
                "wasm"   => run_wasm_step(&client, &step, plan.workdir.as_deref(), &cache_dir).await?,
                other    => bail!("unknown step.kind '{}'", other),
            }
        }

        println!("✓ Local execution complete");
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

// ---- Execute docker ----
async fn run_docker_step(step: &StepSpec, pipeline_workdir: Option<&str>) -> Result<()> {
    if which("docker").is_err() {
        bail!("docker not found on PATH");
    }

    let mut cmd = Command::new("docker");
    cmd.arg("run").arg("--rm");

    // network: default none
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
        if wd.starts_with('/') {
            cmd.args(["-w", wd]);
        } else {
            tracing::warn!("ignoring non-absolute workdir '{}'", wd);
        }
    }

    // entrypoint
    if let Some(ep) = &step.entry {
        cmd.args(["--entrypoint", ep]);
    }

    // image + args
    cmd.arg(&step.ref_);
    for a in &step.args { cmd.arg(a); }

    // spawn + stream
    let mut child = cmd
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning docker for step {}", step.id))?;

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut out_reader = BufReader::new(stdout).lines();
    let mut err_reader = BufReader::new(stderr).lines();

    // OWN the tags so the futures are 'static (don't borrow `step`)
    let tag_out = format!("[{}][stdout] ", step.id);
    let tag_err = format!("[{}][stderr] ", step.id);

    let pump_out = tokio::spawn(async move {
        while let Ok(Some(line)) = out_reader.next_line().await {
            println!("{}{}", tag_out, line);
        }
    });
    let pump_err = tokio::spawn(async move {
        while let Ok(Some(line)) = err_reader.next_line().await {
            eprintln!("{}{}", tag_err, line);
        }
    });

    let status = child.wait().await?;
    let _ = pump_out.await;
    let _ = pump_err.await;

    if !status.success() {
        bail!("step '{}' failed with {}", step.id, status);
    }
    Ok(())
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
