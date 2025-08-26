use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use clap::{ValueEnum};
use tokio::time::{sleep, Duration};
use std::{fs, path::Path};
use serde::{Serialize, Deserialize};
use inquire::{Text, Select, Confirm};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

mod starthub_api;
mod ghapp;
mod config; // 👈 add
mod runners;

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum RunnerKind {
    Github,
    Local, // placeholder for future
}

// ---- Starthub manifest schema ----
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ShManifest {
    name: String,
    version: String,
    kind: ShKind,                 // 👈 new
    repository: String,             // 👈 added
    license: String,                // 👈 added
    inputs: Vec<ShPort>,
    outputs: Vec<ShPort>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ShKind { Wasm, Docker }      // 👈 new

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ShPort {
    name: String,
    description: String,
    #[serde(rename = "type")]
    ty: ShType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ShType {
    String,
    Integer,
    Boolean,
    Object,
    Array,
    Number,
}

// ---------- Docker scaffolding templates ----------
const DOCKERFILE_TPL: &str = r#"FROM alpine:3.20

RUN apk add --no-cache curl jq

WORKDIR /app
COPY entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

CMD ["/app/entrypoint.sh"]
"#;

const ENTRYPOINT_SH_TPL: &str = r#"#!/bin/sh
set -euo pipefail

# Read entire JSON payload from stdin:
INPUT="$(cat || true)"

# Secrets from env first; otherwise from stdin.params (avoid leaking in logs/state)
do_access_token="${do_access_token:-}"
if [ -z "${do_access_token}" ]; then
  do_access_token="$(printf '%s' "$INPUT" | jq -r '(.params.do_access_token // .do_access_token // empty)')"
fi

# Non-secrets from env or stdin.params
do_project_id="${do_project_id:-$(printf '%s' "$INPUT" | jq -r '(.params.do_project_id // .do_project_id // empty)')}"
do_tag_name="${do_tag_name:-$(printf '%s' "$INPUT" | jq -r '(.params.do_tag_name // .do_tag_name // empty)')}"

# Validate
[ -n "${do_access_token:-}" ] || { echo "Error: do_access_token missing (env or stdin.params)" >&2; exit 1; }
[ -n "${do_project_id:-}" ]  || { echo "Error: do_project_id missing (env or stdin.params)"  >&2; exit 1; }
[ -n "${do_tag_name:-}" ]    || { echo "Error: do_tag_name missing (env or stdin.params)"    >&2; exit 1; }

label="starthub-tag:${do_tag_name}"
echo "📝 Updating project ${do_project_id} description to include '${label}'..." >&2

# 1) Fetch current project
get_resp="$(
  curl -sS -f -X GET "https://api.digitalocean.com/v2/projects/${do_project_id}" \
    -H "Authorization: Bearer ${do_access_token}" \
    -H "Content-Type: application/json"
)"

current_desc="$(printf '%s' "$get_resp" | jq -r '.project.description // ""')"

# 2) Build new description idempotently
case "$current_desc" in
  *"$label"*) new_desc="$current_desc" ;;
  "")         new_desc="$label" ;;
  *)          new_desc="$current_desc, $label" ;;
esac

# 3) PATCH only if needed
if [ "$new_desc" = "$current_desc" ]; then
  patch_resp="$get_resp"
else
  patch_resp="$(
    curl -sS -f -X PATCH "https://api.digitalocean.com/v2/projects/${do_project_id}" \
      -H "Authorization: Bearer ${do_access_token}" \
      -H "Content-Type: application/json" \
      -d "$(jq -nc --arg d "$new_desc" '{description:$d}')"
  )"
fi

# 4) Verify success
project_id_parsed="$(printf '%s' "$patch_resp" | jq -r '.project.id // empty')"
[ -n "$project_id_parsed" ] || { echo "❌ Failed to update project"; echo "$patch_resp" | jq . >&2; exit 1; }

# 5) ✅ Emit output that matches the manifest exactly
echo "::starthub:state::{\"do_tag_name\":\"${do_tag_name}\"}"

# 6) Human-readable logs to STDERR
{
  echo "✅ Tag ensured in description. Project ID: ${project_id_parsed}"
  echo "Final description:"
  printf '%s\n' "$patch_resp" | jq -r '.project.description // ""'
} >&2
"#;

const GITIGNORE_TPL: &str = r#"/target
/dist
/node_modules
*.log
*.tmp
.DS_Store
.env
.env.*
starthub.lock.json
"#;

const DOCKERIGNORE_TPL: &str = r#"*
!entrypoint.sh
!starthub.json
!.dockerignore
!.gitignore
!README.md
!Dockerfile
"#;

fn readme_tpl(name: &str, kind: &ShKind, repo: &str, license: &str) -> String {
    let kind_str = match kind { ShKind::Docker => "docker", ShKind::Wasm => "wasm" };
    format!(r#"# {name}

A Starthub **{kind_str}** action.

- Repository: `{repo}`
- License: `{license}`

## Usage

This action reads a JSON payload from **stdin** and prints state as:

::starthub:state::{{"key":"value"}}

Inputs / Outputs

Document your inputs and outputs in starthub.json.
### Build & run (docker)
```bash
docker build -t {name}:dev .
echo '{{"params":{{}}}}' | docker run -i --rm {name}:dev
"#
)
}

// ---------- WASM scaffolding templates ----------
fn wasm_cargo_toml_tpl(name: &str, version: &str) -> String {
    format!(r#"[package]
name = "{name}"
version = "{version}"
edition = "2021"
rust-version = "1.82"
publish = false

[dependencies]
waki = {{ version = "0.4.2", features = ["json", "multipart"] }}
serde = {{ version = "1.0.202", features = ["derive"] }}
serde_json = "1.0"

# reduce wasm binary size
[profile.release]
lto = true
strip = "symbols"
"#)
}

const WASM_MAIN_RS_TPL: &str = r#"use std::io::{self, Read};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use waki::Client;

#[derive(Deserialize)]
struct Input {
    #[serde(default)]
    state: Value,
    #[serde(default)]
    params: Value,
}

fn main() {
    // ---- read stdin ----
    let mut buf = String::new();
    let _ = io::stdin().read_to_string(&mut buf);
    let input: Input = serde_json::from_str(&buf)
        .unwrap_or(Input { state: Value::Null, params: Value::Null });

    // ---- required url ----
    let Some(url) = input.params.get("url").and_then(|v| v.as_str()) else {
        eprintln!("Error: missing required param 'url'");
        return;
    };

    // ---- optional headers (make &'static strs) ----
    let mut headers_static: Vec<(&'static str, &'static str)> = Vec::new();
    if let Some(hmap) = input.params.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in hmap {
            if let Some(val) = v.as_str() {
                let k_static: &'static str = Box::leak(k.clone().into_boxed_str());
                let v_static: &'static str = Box::leak(val.to_string().into_boxed_str());
                headers_static.push((k_static, v_static));
            }
        }
    }

    // ---- GET ----
    let resp = Client::new()
        .get(url)
        .headers(headers_static) // <-- pass Vec, not slice
        .connect_timeout(Duration::from_secs(15))
        .send();

    match resp {
        Ok(r) => {
            let status = r.status_code();
            let body = r.body().unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body).to_string();

            // Emit manifest-style outputs
            println!("::starthub:state::{}", json!({
                "status": status,
                "body": body_str
            }).to_string());

            eprintln!("GET {} -> {}", url, status);
        }
        Err(e) => eprintln!("Request error: {}", e),
    }
}
"#;

// safe write with overwrite prompt
fn write_file_guarded(path: &Path, contents: &str) -> anyhow::Result<()> {
if path.exists() {
let overwrite = Confirm::new(&format!("{} exists. Overwrite?", path.display()))
.with_default(false)
.prompt()?;
if !overwrite { return Ok(()); }
}
fs::write(path, contents)?;
Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> anyhow::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> anyhow::Result<()> { Ok(()) }

#[derive(Parser, Debug)]
#[command(name="starthub", version, about="Starthub CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    /// Verbose logs
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Initialize a project (creates config, etc.)
    Init {
        #[arg(long, default_value = ".")]
        path: String,
    },
    /// Deploy with the given config
    Run {
        /// Package slug/name, e.g. "chirpstack"
        action: String,       
        /// Repeatable env secret: -e KEY=VALUE (will become a repo secret)
        #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
        secrets: Vec<String>,                    // <— collect multiple -e
        /// Choose where to run the deployment
        #[arg(long, value_enum, default_value_t = RunnerKind::Github)]
        runner: RunnerKind,
        /// Optional environment name
        #[arg(long)]
        env: Option<String>,
    },
    /// Show deployment status
    Status {
        #[arg(long)]
        id: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose { "info" } else { "warn" };
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("STARTHUB_LOG").unwrap_or_else(|_| filter.into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    match cli.command {
        Commands::Init { path } => cmd_init(path).await?,
        Commands::Run { action, secrets, env, runner } => cmd_run(action, secrets, env, runner).await?,
        Commands::Status { id } => cmd_status(id).await?,
    }
    Ok(())
}

// ------------------- cmd_init -------------------
async fn cmd_init(path: String) -> anyhow::Result<()> {
    // Basic fields
    let name = Text::new("Package name:")
    .with_default("http-get-wasm")
    .prompt()?;

    let version = Text::new("Version:")
        .with_default("0.0.1")
        .prompt()?;

    // Kind
    let kind_str = Select::new("Kind:", vec!["wasm", "docker"]).prompt()?;
    let kind = match kind_str {
        "wasm" => ShKind::Wasm,
        "docker" => ShKind::Docker,
        _ => unreachable!(),
    };

    // Repository
    let repo_default = match kind {
        ShKind::Wasm   => "github.com/starthubhq/http-get-wasm",
        ShKind::Docker => "ghcr.io/starthubhq/http-get-wasm",
    };
    let repository = Text::new("Repository:")
        .with_help_message(match kind {
            ShKind::Wasm => "Git repo URL or owner/repo (for source of truth)",
            ShKind::Docker => "OCI image path without tag (e.g., ghcr.io/org/image)",
        })
        .with_default(repo_default)
        .prompt()?;

    // License
    let license = Select::new("License:", vec![
        "Apache-2.0", "MIT", "BSD-3-Clause", "GPL-3.0", "Unlicense", "Proprietary",
    ]).prompt()?.to_string();

    // Empty I/O (you can extend later)
    let inputs: Vec<ShPort> = Vec::new();
    let outputs: Vec<ShPort> = Vec::new();

    // Manifest
    let manifest = ShManifest { name: name.clone(), version: version.clone(), kind: kind.clone(), repository: repository.clone(), license: license.clone(), inputs, outputs };
    let json = serde_json::to_string_pretty(&manifest)?;

    // Ensure dir
    let out_dir = Path::new(&path);
    if !out_dir.exists() {
        fs::create_dir_all(out_dir)?;
    }

    // Write starthub.json
    write_file_guarded(&out_dir.join("starthub.json"), &json)?;
    // Always create .gitignore / .dockerignore / README.md
    write_file_guarded(&out_dir.join(".gitignore"), GITIGNORE_TPL)?;
    write_file_guarded(&out_dir.join(".dockerignore"), DOCKERIGNORE_TPL)?;
    let readme = readme_tpl(&name, &kind, &repository, &license);
    write_file_guarded(&out_dir.join("README.md"), &readme)?;

    // If docker, scaffold Dockerfile + entrypoint.sh
    if matches!(kind, ShKind::Docker) {
        let dockerfile = out_dir.join("Dockerfile");
        write_file_guarded(&dockerfile, DOCKERFILE_TPL)?;
        let entrypoint = out_dir.join("entrypoint.sh");
        write_file_guarded(&entrypoint, ENTRYPOINT_SH_TPL)?;
        make_executable(&entrypoint)?;
    }

    // If wasm, scaffold Cargo.toml + src/main.rs
    if matches!(kind, ShKind::Wasm) {
        let cargo = out_dir.join("Cargo.toml");
        write_file_guarded(&cargo, &wasm_cargo_toml_tpl(&name, &version))?;

        let src_dir = out_dir.join("src");
        if !src_dir.exists() {
            fs::create_dir_all(&src_dir)?;
        }
        let main_rs = src_dir.join("main.rs");
        write_file_guarded(&main_rs, WASM_MAIN_RS_TPL)?;
    }

    println!("✓ Wrote {}", out_dir.join("starthub.json").display());
    Ok(())
}


async fn cmd_login(runner: RunnerKind) -> anyhow::Result<()> {
    let r = make_runner(runner);
    println!("→ Logging in for runner: {}", r.name());
    r.ensure_auth().await?;
    println!("✓ Login complete for {}", r.name());
    Ok(())
}

// Parse KEY=VALUE items into Vec<(String,String)>, with friendly errors.
fn parse_secret_pairs(items: &[String]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for raw in items {
        let (k, v) = raw
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!(format!("invalid -e value '{raw}', expected KEY=VALUE")))?;
        if k.trim().is_empty() {
            anyhow::bail!("secret name empty in '{raw}'");
        }
        out.push((k.trim().to_string(), v.to_string()));
    }
    Ok(out)
}

fn open_actions_page(owner: &str, repo: &str) {
    let url = format!("https://github.com/{owner}/{repo}/actions");
    match webbrowser::open(&url) {
        Ok(_) => println!("↗ Opened GitHub Actions: {url}"),
        Err(e) => println!("→ GitHub Actions: {url} (couldn't auto-open: {e})"),
    }
}

async fn cmd_run(action: String, secrets: Vec<String>, env: Option<String>, runner: RunnerKind) -> Result<()> {
    let parsed_secrets = parse_secret_pairs(&secrets)?;
    let mut ctx = runners::DeployCtx {
        action,
        env,
        owner: None,
        repo: None,
        secrets: parsed_secrets,       // <— pass to runner
    };
    let r = make_runner(runner);

    // 1) ensure auth for selected runner; guide if missing
    r.ensure_auth().await?;

    // 2) do the runner-specific steps
    r.prepare(&mut ctx).await?;
    r.put_files(&ctx).await?;
    r.set_secrets(&ctx).await?;       // <— will create repo secrets
    r.dispatch(&ctx).await?;

    if let (Some(owner), Some(repo)) = (ctx.owner.as_deref(), ctx.repo.as_deref()) {
        sleep(Duration::from_secs(5)).await;
        open_actions_page(owner, repo);
    }

    println!("✓ Dispatch complete for {}", r.name());

    
    Ok(())
}

async fn cmd_status(id: Option<String>) -> Result<()> {
    println!("Status for {id:?}");
    // TODO: poll API
    Ok(())
}

pub fn make_runner(kind: RunnerKind) -> Box<dyn runners::Runner + Send + Sync> {
    match kind {
        RunnerKind::Github => Box::new(runners::github::GithubRunner),
        RunnerKind::Local  => Box::new(runners::local::LocalRunner),
    }
}