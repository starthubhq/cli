use anyhow::Result;
use clap::{Parser, Subcommand};
use axum::{routing::{get, post}, Router, extract::State, Json};
use std::sync::{Arc, Mutex};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use clap::{ValueEnum};
use tokio::time::{sleep, Duration};
use std::{fs, path::Path};
use std::process::Command as PCommand;
use serde::{Serialize, Deserialize};
use serde_json::Value as JsonValue;
use inquire::{Text, Select, Confirm};
use tokio::sync::oneshot;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use axum::{
    body::Body,
    http::{header::CONTENT_TYPE, StatusCode, Uri},
    response::Response,
};
use rust_embed::RustEmbed;
use mime_guess::from_path;


#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

mod starthub_api;
mod ghapp;
mod config; // 👈 add
mod runners;

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RunnerKind {    
    Github,
    #[default] // <- this one is the default
    Local, // placeholder for future
}

// ---- Starthub manifest schema ----
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ShManifest {
    name: String,
    version: String,
    kind: ShKind,                 // 👈 new
    repository: String,             // 👈 added
    image: String,
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
struct ShLock {
    name: String,
    version: String,
    kind: ShKind,
    distribution: ShDistribution,
    digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ShDistribution {
    primary: String,                // oci://ghcr.io/org/image@sha256:...
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream: Option<String>,       // keep for future mirrors; None for now
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
    Publish {
        /// Do not build, only push/tag (assumes image exists locally)
        #[arg(long)]
        no_build: bool,
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
        Commands::Publish { no_build } => cmd_publish(no_build).await?,   // 👈
        Commands::Run { action, secrets: _, env: _, runner } => cmd_run(action, runner).await?,
        Commands::Status { id } => cmd_status(id).await?,
    }
    Ok(())
}

async fn cmd_publish(no_build: bool) -> anyhow::Result<()> {
    let manifest_str = fs::read_to_string("starthub.json")?;
    let m: ShManifest = serde_json::from_str(&manifest_str)?;

    match m.kind {
        ShKind::Docker => cmd_publish_docker_inner(&m, no_build).await,
        ShKind::Wasm   => cmd_publish_wasm_inner(&m, no_build).await,
    }
}

// Parse any "digest: sha256:..." occurrence (push or inspect output)
fn parse_digest_any(s: &str) -> Option<String> {
    for line in s.lines() {
        // be forgiving on casing
        let lower = line.to_ascii_lowercase();
        if let Some(pos) = lower.find("digest:") {
            // slice the original line at the same offset to avoid lossy lowercasing of the digest itself
            let rest = line[(pos + "digest:".len())..].trim();

            // find the sha256 token in the remainder
            if let Some(idx) = rest.find("sha256:") {
                let token = &rest[idx..];
                // token may be followed by spaces/text (e.g., " size: 1361")
                let digest = token.split_whitespace().next().unwrap_or("");
                if digest.starts_with("sha256:") && digest.len() == "sha256:".len() + 64 {
                    return Some(digest.to_string());
                }
            }
        }
    }
    None
}

fn push_and_get_digest(tag: &str) -> anyhow::Result<String> {
    // 1) docker push (capture output; push often prints the digest line)
    let push_out = run_capture("docker", &["push", tag])?;
    if let Some(d) = parse_digest_any(&push_out) {
        return Ok(d);
    }

    // 2) fallback: docker buildx imagetools inspect
    if let Ok(inspect_out) = run_capture("docker", &["buildx", "imagetools", "inspect", tag]) {
        if let Some(d) = parse_digest_any(&inspect_out) {
            return Ok(d);
        }
    }

    // 3) optional: crane digest (if installed)
    if let Ok(crane_out) = run_capture("crane", &["digest", tag]) {
        let d = crane_out.trim().to_string();
        if d.starts_with("sha256:") && d.len() == "sha256:".len() + 64 {
            return Ok(d);
        }
    }

    anyhow::bail!("could not parse image digest from `docker push`/`imagetools` output; ensure `docker buildx` is available or install `crane`.")
}

fn oci_from_manifest(m: &ShManifest) -> anyhow::Result<String> {
    // 1) If you still carry `ref` in your JSON, prefer it when it's an OCI path without scheme
    // (Optional: add it to ShManifest as Option<String>)
    // try to parse a repo-like value from m.repository
    let repo = m.repository.trim();

    // If user gave a proper OCI path already (ghcr.io/..., docker.io/..., etc.)
    if repo.starts_with("ghcr.io/")
        || repo.starts_with("docker.io/")
        || repo.starts_with("registry-1.docker.io/")
        || repo.split('/').count() >= 2 && !repo.starts_with("http")
    {
        return Ok(repo.trim_end_matches('/').to_string());
    }

    // GitHub URL → map to GHCR
    if repo.starts_with("https://github.com/") || repo.starts_with("github.com/") {
        let parts: Vec<&str> = repo.trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("github.com/")
            .split('/')
            .collect();
        if parts.len() >= 2 {
            let org = parts[0];
            let name = parts[1].trim_end_matches(".git");
            return Ok(format!("ghcr.io/{}/{}", org, name));
        }
    }

    anyhow::bail!("For docker kind, please set `repository` to an OCI image base like `ghcr.io/<org>/<image>` or a GitHub repo URL so I can map it to GHCR.");
}

async fn cmd_publish_docker_inner(m: &ShManifest, no_build: bool) -> anyhow::Result<()> {
    let image_base = oci_from_manifest(m)?;           // uses m.image or maps GitHub → ghcr
    let tag = format!("{}:{}", image_base, m.version);

    if !no_build {
        run("docker", &["build", "-t", &tag, "."])?;
    }

    let digest = push_and_get_digest(&tag)?;         // parses digest from `docker push` output

    let primary = format!("oci://{}@{}", image_base, &digest);
    let lock = ShLock {
        name: m.name.clone(),
        version: m.version.clone(),
        kind: m.kind.clone(),
        distribution: ShDistribution { primary, upstream: None },
        digest,
    };
    fs::write("starthub.lock.json", serde_json::to_string_pretty(&lock)?)?;

    println!("✓ Pushed {tag}");
    println!("✓ Wrote starthub.lock.json");
    Ok(())
}

fn push_wasm_and_get_digest(tag: &str, wasm_path: &str) -> anyhow::Result<String> {
    // oras push ghcr.io/org/pkg:vX.Y.Z module.wasm:application/wasm
    let push_out = run_capture(
        "oras",
        &["push", tag, &format!("{}:application/wasm", wasm_path)],
    )?;

    if let Some(d) = parse_digest_any(&push_out) {
        return Ok(d);
    }

    // fallback to crane digest (works with artifact tags too)
    if let Ok(crane_out) = run_capture("crane", &["digest", tag]) {
        let d = crane_out.trim().to_string();
        if d.starts_with("sha256:") && d.len() == "sha256:".len() + 64 {
            return Ok(d);
        }
    }

    anyhow::bail!(
        "could not parse digest from `oras push` output; \
         ensure `oras` (and optionally `crane`) are installed and the tag exists"
    )
}

fn find_wasm_artifact(crate_name: &str) -> Option<String> {
    use std::ffi::OsStr;

    let name_dash = crate_name.to_string();
    let name_underscore = crate_name.replace('-', "_");

    let candidate_dirs = [
        "target/wasm32-wasi/release",
        "target/wasm32-wasi/release/deps",
        "target/wasm32-wasip1/release",
        "target/wasm32-wasip1/release/deps",
    ];

    // 1) Try exact filenames first (dash & underscore)
    for dir in &candidate_dirs {
        for fname in [
            format!("{}/{}.wasm", dir, name_dash),
            format!("{}/{}.wasm", dir, name_underscore),
        ] {
            if Path::new(&fname).exists() {
                return Some(fname);
            }
        }
    }

    // 2) Fallback: pick the newest *.wasm in the candidate dirs
    let mut newest: Option<(std::time::SystemTime, String)> = None;
    for dir in &candidate_dirs {
        if let Ok(rd) = fs::read_dir(dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                if path.extension() == Some(OsStr::new("wasm")) {
                    if let Ok(meta) = entry.metadata() {
                        if let Ok(modified) = meta.modified() {
                            let pstr = path.to_string_lossy().to_string();
                            // Prefer files that contain the crate name (dash or underscore)
                            let contains_name = pstr.contains(&name_dash) || pstr.contains(&name_underscore);
                            let score_time = (modified, pstr.clone());
                            match &mut newest {
                                None => newest = Some(score_time),
                                Some((t, _)) if modified > *t => newest = Some(score_time),
                                _ => {}
                            }
                            // If it's clearly our crate, short-circuit
                            if contains_name {
                                return Some(pstr);
                            }
                        }
                    }
                }
            }
        }
    }
    newest.map(|(_, p)| p)
}

// Map a GitHub repo URL/SSH to a GHCR image base
fn github_to_ghcr_path(repo: &str) -> Option<String> {
    let r = repo.trim().trim_end_matches(".git");

    // ssh: git@github.com:owner/repo(.git)?
    if r.starts_with("git@github.com:") {
        let rest = r.trim_start_matches("git@github.com:");
        let mut it = rest.split('/');
        if let (Some(owner), Some(name)) = (it.next(), it.next()) {
            return Some(format!("ghcr.io/{}/{}", owner.to_lowercase(), name.to_lowercase()));
        }
    }

    // https://github.com/owner/repo or http://...
    if r.contains("github.com/") {
        let after = r.splitn(2, "github.com/").nth(1)?;
        let mut it = after.split('/');
        if let (Some(owner), Some(name)) = (it.next(), it.next()) {
            return Some(format!("ghcr.io/{}/{}", owner.to_lowercase(), name.trim_end_matches(".git").to_lowercase()));
        }
    }

    // bare github.com/owner/repo
    if r.starts_with("github.com/") {
        let mut it = r.trim_start_matches("github.com/").split('/');
        if let (Some(owner), Some(name)) = (it.next(), it.next()) {
            return Some(format!("ghcr.io/{}/{}", owner.to_lowercase(), name.to_lowercase()));
        }
    }

    None
}

// Derive an OCI image base for both docker/wasm publish.
// Priority: manifest.image -> manifest.repository (mapped if GitHub) -> git remote origin (mapped)
fn derive_image_base(m: &ShManifest, cli_image: Option<String>) -> anyhow::Result<String> {
    if let Some(i) = cli_image {
        let i = i.trim();
        if !i.is_empty() { return Ok(i.trim_end_matches('/').to_string()); }
    }

    let img = m.image.trim();
    if !img.is_empty() && !img.starts_with("http") && img.split('/').count() >= 2 {
        return Ok(img.trim_end_matches('/').to_string());
    }

    if let Some(mapped) = github_to_ghcr_path(&m.repository) {
        return Ok(mapped);
    }

    // Try `git remote get-url origin`
    if let Ok(out) = PCommand::new("git").args(["remote", "get-url", "origin"]).output() {
        if out.status.success() {
            let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if let Some(mapped) = github_to_ghcr_path(&url) {
                return Ok(mapped);
            }
        }
    }

    anyhow::bail!(
        "Unable to determine OCI image path. Provide one with `--image ghcr.io/<org>/<name>` \
         or set `image` in starthub.json, or set `repository` to a GitHub URL I can map."
    )
}

// Write starthub.lock.json with the digest-pinned primary ref
fn write_lock(m: &ShManifest, image_base: &str, digest: &str) -> anyhow::Result<()> {
    let primary = format!("oci://{}@{}", image_base, digest);
    let lock = ShLock {
        name: m.name.clone(),
        version: m.version.clone(),
        kind: m.kind.clone(),
        distribution: ShDistribution { primary, upstream: None },
        digest: digest.to_string(),
    };
    fs::write("starthub.lock.json", serde_json::to_string_pretty(&lock)?)?;
    Ok(())
}

async fn cmd_publish_wasm_inner(m: &ShManifest, no_build: bool) -> anyhow::Result<()> {
    let image_base = derive_image_base(m, None)?; // same resolver you use for docker/wasm
    let tag = format!("{}:v{}", image_base, m.version);

    if !no_build {
        // Try cargo-component (component model) first; fall back to plain WASI target.
        // Ignore rustup failure if target already installed.
        let _ = run("rustup", &["target", "add", "wasm32-wasi"]);
        // Prefer cargo-component if available
        if run("cargo", &["+nightly", "component", "--version"]).is_ok() {
            run("cargo", &["+nightly", "component", "build", "--release"])?;
        } else {
            run("cargo", &["build", "--release", "--target", "wasm32-wasi"])?;
        }
    }

    // Find the .wasm produced by the build
    let wasm_path = find_wasm_artifact(&m.name)
        .ok_or_else(|| anyhow::anyhow!("WASM build artifact not found; looked in `target/**/release/**/*.wasm`"))?;

    // Push to OCI registry as an artifact
    let digest = push_wasm_and_get_digest(&tag, &wasm_path)?;

    // Lockfile
    write_lock(m, &image_base, &digest)?;
    println!("✓ Pushed {tag}\n✓ Wrote starthub.lock.json");
    Ok(())
}
fn run(cmd: &str, args: &[&str]) -> anyhow::Result<()> {
    match PCommand::new(cmd).args(args).status() {
        Ok(status) => {
            anyhow::ensure!(status.success(), "command failed: {} {:?}", cmd, args);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("`{}` not found. Install it first (e.g., `brew install {}`)", cmd, cmd)
        }
        Err(e) => Err(e.into()),
    }
}

fn run_capture(cmd: &str, args: &[&str]) -> anyhow::Result<String> {
    match PCommand::new(cmd).args(args).output() {
        Ok(out) => {
            anyhow::ensure!(out.status.success(), "command failed: {} {:?}", cmd, args);
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("`{}` not found. Install it first (e.g., `brew install {}`)", cmd, cmd)
        }
        Err(e) => Err(e.into()),
    }
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
            ShKind::Wasm => "Git repo URL",
            ShKind::Docker => "Git repo URL",
        })
        .with_default(repo_default)
        .prompt()?;

        // After `repository` is collected in cmd_init:
    let default_image = if matches!(kind, ShKind::Docker) {
        // already an OCI by default; user can edit
        repo_default.to_string()
    } else {
        // WASM: map GitHub → GHCR by default for image
        if repository.starts_with("https://github.com/") || repository.starts_with("github.com/") {
            let trimmed = repository
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .trim_start_matches("github.com/");
            let mut parts = trimmed.split('/');
            if let (Some(org), Some(name)) = (parts.next(), parts.next()) {
                format!("ghcr.io/{}/{}", org, name.trim_end_matches(".git"))
            } else {
                "ghcr.io/owner/package".to_string()
            }
        } else {
            "ghcr.io/owner/package".to_string()
        }
    };

    let image = Text::new("Image (OCI path, no tag):")
        .with_help_message("e.g., ghcr.io/org/package")
        .with_default(&default_image)
        .prompt()?;

    // License
    let license = Select::new("License:", vec![
        "Apache-2.0", "MIT", "BSD-3-Clause", "GPL-3.0", "Unlicense", "Proprietary",
    ]).prompt()?.to_string();

    // Empty I/O (you can extend later)
    let inputs: Vec<ShPort> = Vec::new();
    let outputs: Vec<ShPort> = Vec::new();

    // Manifest
    let manifest = ShManifest { name: name.clone(), version: version.clone(), kind: kind.clone(), repository: repository.clone(), image: image.clone(), license: license.clone(), inputs, outputs };
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
    // .dockerignore only for Docker projects
    if matches!(kind, ShKind::Docker) {
        write_file_guarded(&out_dir.join(".dockerignore"), DOCKERIGNORE_TPL)?;
    }
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






fn open_actions_page(owner: &str, repo: &str) {
    let url = format!("https://github.com/{owner}/{repo}/actions");
    match webbrowser::open(&url) {
        Ok(_) => println!("↗ Opened GitHub Actions: {url}"),
        Err(e) => println!("→ GitHub Actions: {url} (couldn't auto-open: {e})"),
    }
}

#[derive(RustEmbed)]
#[folder = "ui/dist/"]     // embedded at compile time
struct Assets;

async fn embedded_assets(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Try requested path directly (for real assets like *.js, *.css, *.png)
    let candidate = if path.is_empty() { "index.html" } else { path };
    let asset = Assets::get(candidate);

    if let Some(content) = asset {
        let body = Body::from(content.data.into_owned());
        let mime = from_path(candidate).first_or_octet_stream();
        return Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, mime.as_ref())
            .body(body.into())
            .unwrap();
    }

    // 👉 If not found, always serve index.html with text/html (SPA fallback)
    if let Some(index) = Assets::get("index.html") {
        let body = Body::from(index.data.into_owned());
        return Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "text/html")
            .body(body.into())
            .unwrap();
    }

    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(axum::body::Body::empty())
        .unwrap()
}

// Shared app state to deliver the first POSTed RunRequest to cmd_run
#[derive(Clone)]
struct AppState {
    tx: Arc<Mutex<Option<oneshot::Sender<RunRequest>>>>,
}

// ---------- API types ----------
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunRequest {
    action: String,
    #[serde(default)]
    env: Option<String>,
    #[serde(default)]
    runner: RunnerKind,
    /// Arbitrary inputs keyed by input-port name
    #[serde(default)]
    inputs: JsonValue,     
    /// secrets as { "KEY": "VALUE", ... }
    #[serde(default)]
    secrets: std::collections::HashMap<String, String>,
}

fn parse_action_ref(s: &str) -> Option<(String, String, Option<String>)> {
    // Accept: ns/slug@1.2.3 | ns/slug | slug (fallback → no ns)
    let (path, ver) = match s.split_once('@') {
        Some((p, v)) if !v.is_empty() => (p, Some(v.to_string())),
        _ => (s, None),
    };

    // ns/slug
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    match (parts.next(), parts.next(), parts.next()) {
        (Some(ns), Some(slug), None) => Some((ns.to_string(), slug.to_string(), ver)),
        // allow single `slug` (no namespace) if you want → map to just `/slug` (or return None)
        (Some(slug_only), None, None) => Some(("".to_string(), slug_only.to_string(), ver)),
        _ => None,
    }
}

// ---------- API router ----------
fn api_router(state: AppState) -> Router {
    async fn health() -> &'static str { "ok" }

    async fn post_run(
        State(state): State<AppState>,
        Json(req): Json<RunRequest>,
    ) -> Result<Json<serde_json::Value>, StatusCode> {
        if let Some(tx) = state.tx.lock().unwrap().take() {
            // deliver the request to cmd_run
            let _ = tx.send(req);
            Ok(Json(serde_json::json!({ "ok": true })))
        } else {
            // already consumed
            Err(StatusCode::CONFLICT)
        }
    }

    Router::new()
        .route("/health", get(health))
        .route("/run", post(post_run))
        .with_state(state)
}


async fn cmd_run(action: String, _runner: RunnerKind) -> Result<()> {
    // oneshot to receive the first /api/run payload
    let (tx, rx) = oneshot::channel::<RunRequest>();
    let state = AppState { tx: Arc::new(Mutex::new(Some(tx))) };

    // Build the app: API + SPA
    let app = Router::new()
        .nest("/api", api_router(state.clone()))
        .fallback(embedded_assets);

    let addr: SocketAddr = ([127, 0, 0, 1], 8888).into();
    let listener = TcpListener::bind(addr).await.unwrap();

    println!("UI at http://{}", addr);

    // Decide which URL to open (deep-link if action is ns/slug[@version])
    let start_url = match parse_action_ref(&action) {
        Some((ns, slug, ver)) if !ns.is_empty() => match ver {
            Some(v) => format!("http://localhost:8888/{}/{}/{}", ns, slug, v),
            None    => format!("http://localhost:8888/{}/{}", ns, slug),
        },
        Some((_, slug, ver)) => match ver { // no namespace case: /<slug>[/<version>]
            Some(v) => format!("http://localhost:8888/{}/{}", slug, v),
            None    => format!("http://localhost:8888/{}", slug),
        },
        None => "http://localhost:8888/".to_string(), // fallback
    };

    println!("Opening browser at {start_url}");

    tokio::spawn({
        let start_url = start_url.clone();
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let _ = webbrowser::open(&start_url);
        }
    });

    // Run the server in the background
    let _server = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service()).await.unwrap();
    });

    println!("Waiting for input from the UI… (POST /api/run)");

    // Wait for the first /api/run
    let req = rx.await.map_err(|_| anyhow::anyhow!("server channel closed"))?;
    println!("→ Received run request from UI: action={} runner={:?} env={:?}", req.action, req.runner, req.env);

    // Build your deployment context from req (replace current TODO block)
    let parsed_secrets: Vec<(String, String)> = req.secrets.into_iter().collect();

    let mut ctx = runners::DeployCtx {
        action: req.action,
        env: req.env,
        owner: None,
        repo: None,
        secrets: parsed_secrets,
        // if you want to pass inputs to the runner, add a field in DeployCtx:
        // inputs: req.inputs,  // (change DeployCtx accordingly)
    };

    let r = make_runner(req.runner);

    // 1) ensure auth for selected runner
    r.ensure_auth().await?;
    // 2) runner-specific steps
    r.prepare(&mut ctx).await?;
    r.put_files(&ctx).await?;
    r.set_secrets(&ctx).await?;
    r.dispatch(&ctx).await?;

    if let (Some(owner), Some(repo)) = (ctx.owner.as_deref(), ctx.repo.as_deref()) {
        sleep(Duration::from_secs(5)).await;
        open_actions_page(owner, repo);
    }

    println!("✓ Dispatch complete for {}", r.name());

    // let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    // let server = tokio::spawn(async move {
    //     axum::serve(listener, app.into_make_service())
    //         .with_graceful_shutdown(async {
    //             let _ = shutdown_rx.await; // wait for signal
    //         })
    //         .await
    //         .unwrap();
    // });

    // After dispatch completes, stop the server:
    // let _ = shutdown_tx.send(());

    // If you want to wait for clean shutdown:
    // let _ = server.await;

    // Optionally: stop the server by exiting the process here,
    // or keep it running by awaiting it (will block):
    // server.await.ok();

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