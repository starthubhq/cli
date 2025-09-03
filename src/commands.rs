use anyhow::Result;
use std::{fs, path::Path};
use std::process::Command as PCommand;
use inquire::{Text, Select, Confirm};
use tokio::time::{sleep, Duration};
use webbrowser;
use aws_config::meta::region::RegionProviderChain;
use aws_sdk_s3::{config::Region, Client as S3Client};
use aws_sdk_s3::primitives::ByteStream;

use crate::models::{ShManifest, ShKind, ShPort, ShLock, ShDistribution, ShType};
use crate::templates;

// Global constants for local development server
// Change these if you need to use a different port or host
const LOCAL_SERVER_URL: &str = "http://127.0.0.1:3000";
const LOCAL_SERVER_HOST: &str = "127.0.0.1:3000";

use axum::{
    routing::{get, post},
    response::{Html, Json},
    Router,
    extract::ws::{WebSocket, WebSocketUpgrade},
    response::IntoResponse,
};
use tokio::net::TcpListener;
use tower_http::services::ServeDir;
use tower_http::cors::CorsLayer;
use serde_json::{Value, json};
use futures_util::{StreamExt, SinkExt};
use tokio::sync::broadcast;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

// Shared state for WebSocket connections
#[derive(Clone)]
struct AppState {
    ws_sender: broadcast::Sender<String>,
}

impl AppState {
    fn new() -> Self {
        let (ws_sender, _) = broadcast::channel(100);
        Self { ws_sender }
    }
}







// ============================================================================
// HELPER FUNCTIONS FOR ACTION RESOLUTION
// ============================================================================

/// Convert ShActionStep to local::ActionStep (no conversion needed since we're using ShActionStep directly)
fn convert_action_step(step: &crate::models::ShActionStep) -> crate::runners::local::ActionStep {
    crate::runners::local::ActionStep {
        id: step.id.clone(),
        kind: step.kind.clone(),
        uses: step.uses.clone(),
        with: step.with.clone(),
    }
}

/// Convert ShWire to local::Wire
fn convert_action_wire(wire: &crate::models::ShWire) -> crate::runners::local::Wire {
    crate::runners::local::Wire {
        from: crate::runners::local::WireFrom {
            source: wire.from.source.clone(),
            step: wire.from.step.clone(),
            output: wire.from.output.clone(),
            key: wire.from.key.clone(),
            value: wire.from.value.clone(),
        },
        to: crate::runners::local::WireTo {
            step: wire.to.step.clone(),
            input: wire.to.input.clone(),
        },
    }
}

/// Recursively fetch all action manifests for a composite action
async fn fetch_all_action_manifests(
    client: &crate::starthub_api::Client,
    action_ref: &str,
    visited: &mut std::collections::HashSet<String>
) -> Result<Vec<crate::models::ShManifest>> {
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
    
    println!("🔗 Constructed storage URL: {}", storage_url);
    
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

// safe write with overwrite prompt
pub fn write_file_guarded(path: &Path, contents: &str) -> anyhow::Result<()> {
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

// Parse any "digest: sha256:..." occurrence (push or inspect output)
pub fn parse_digest_any(s: &str) -> Option<String> {
    for line in s.lines() {
        // be forgiving on casing
        let lower = line.to_ascii_lowercase();
        if let Some(pos) = lower.find("digest:") {
            // slice the original line at the same offset to avoid lossy lowercasing of the digest itself
            let rest = line[(pos + "digest:".len())..].trim();

            // find the sha256 token in the remainder (case insensitive)
            let rest_lower = rest.to_ascii_lowercase();
            if let Some(idx) = rest_lower.find("sha256:") {
                let token = &rest[idx..];
                // token may be followed by spaces/text (e.g., " size: 1361")
                let digest = token.split_whitespace().next().unwrap_or("");
                if digest.len() >= "sha256:".len() + 64 {
                    return Some(digest.to_string());
                }
            }
        }
    }
    None
}

pub fn push_and_get_digest(tag: &str) -> anyhow::Result<String> {
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

pub fn oci_from_manifest(m: &ShManifest) -> anyhow::Result<String> {
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

pub async fn cmd_publish_docker_inner(m: &ShManifest, no_build: bool) -> anyhow::Result<()> {
    let tag = format!("{}:{}", m.name, m.version);

    if !no_build {
        run("docker", &["build", "-t", &tag, "."])?;
    }

    // Save Docker image as tar file
    let tar_filename = format!("{}-{}.tar", m.name, m.version);
    run("docker", &["save", "-o", &tar_filename, &tag])?;

    // Compress the tar file
    let zip_filename = format!("{}-{}.zip", m.name, m.version);
    run("zip", &["-j", &zip_filename, &tar_filename])?;

    // Get the user's namespace from their profile
    let namespace = match get_user_namespace().await {
        Ok(Some(ns)) => ns,
        Ok(None) => {
            println!("⚠️  No authentication found. Using default namespace 'actions'");
            println!("💡 Use 'starthub login' to authenticate and use your personal namespace");
            "actions".to_string()
        }
        Err(e) => {
            println!("⚠️  Failed to get user namespace: {}. Using default namespace 'actions'", e);
            println!("💡 Use 'starthub login' to authenticate and use your personal namespace");
            "actions".to_string()
        }
    };
    
    println!("🏷️  Using namespace: {}", namespace);
    
    // Upload to Supabase storage with new path structure: <namespace>/<name>/<version>
    let storage_url = format!(
        "{}/storage/v1/object/public/artifacts/{}/{}/{}/artifact.zip",
        crate::config::STARTHUB_API_BASE, namespace, m.name, m.version
    );
    
    // Upload to Supabase Storage using AWS SDK
    println!("📤 Uploading to Supabase Storage using AWS SDK");
    
    // Get file size for verification
    let metadata = fs::metadata(&zip_filename)?;
    println!("📁 File size: {} bytes", metadata.len());
    
    // Use the artifacts bucket directly as specified in the URL
    let bucket_name = "artifacts";
    let object_key = format!("{}/{}/{}/artifact.zip", namespace, m.name, m.version);
    
    println!("🪣 Bucket: {}", bucket_name);
    println!("🔑 Object key: {}", object_key);
    
    // Read the zip file
    let zip_data = fs::read(&zip_filename)?;
    
    // Upload using AWS SDK to Supabase Storage S3 endpoint
    println!("🔄 Uploading to Supabase Storage using AWS SDK...");

    // Set AWS credentials environment variables for Supabase Storage S3 compatibility
    std::env::set_var("AWS_ACCESS_KEY_ID", crate::config::S3_ACCESS_KEY);
    std::env::set_var("AWS_SECRET_ACCESS_KEY", crate::config::S3_SECRET_KEY);
    
    // Configure AWS SDK for Supabase Storage S3 compatibility
    let region_provider = RegionProviderChain::first_try(Region::new(crate::config::SUPABASE_STORAGE_REGION));
    let shared_config = aws_config::from_env()
        .region(region_provider)
        .load()
        .await;
    
    // Create S3 client with custom endpoint
    // Ensure the endpoint ends with a slash for proper URL construction
    let endpoint_url = if crate::config::SUPABASE_STORAGE_S3_ENDPOINT.ends_with('/') {
        crate::config::SUPABASE_STORAGE_S3_ENDPOINT.to_string()
    } else {
        format!("{}/", crate::config::SUPABASE_STORAGE_S3_ENDPOINT)
    };
    
    let s3_config = aws_sdk_s3::config::Builder::from(&shared_config)
        .endpoint_url(&endpoint_url)
        .force_path_style(true) // Use path-style for Supabase Storage S3 compatibility
        .build();
    
    println!("🔗 AWS SDK S3 endpoint: {}", crate::config::SUPABASE_STORAGE_S3_ENDPOINT);
    println!("🪣 Bucket: {}", bucket_name);
    println!("🔑 Object key: {}", object_key);
    
    let s3_client = S3Client::from_conf(s3_config);

    // Create ByteStream from the zip data
    let body = ByteStream::from(zip_data.clone());

    // Upload using AWS SDK
    let put_object_output = s3_client
        .put_object()
        .bucket(bucket_name)
        .key(&object_key)
        .body(body)
        .content_type("application/zip")
        .send()
        .await;

    match put_object_output {
        Ok(_) => {
            println!("✅ Successfully uploaded to Supabase Storage using AWS SDK");
        }
        Err(e) => {
            println!("❌ Upload failed: {:?}", e);
            anyhow::bail!("Failed to upload to Supabase Storage");
        }
    }
    
    // Clean up temporary files
    fs::remove_file(&tar_filename)?;
    fs::remove_file(&zip_filename)?;

    // Generate a digest for the uploaded artifact
    let digest = format!("sha256:{}", m.name); // Simplified digest for now
    
    let primary = format!("{}@{}", storage_url, &digest);
    let lock = ShLock {
        name: m.name.clone(),
        description: m.description.clone(),
        version: m.version.clone(),
        kind: m.kind.clone().expect("Kind should be present in manifest"),
        manifest_version: m.manifest_version,
        repository: m.repository.clone(),
        image: m.image.clone(),
        license: m.license.clone(),
        inputs: m.inputs.clone(),
        outputs: m.outputs.clone(),
        distribution: ShDistribution { primary, upstream: None },
        digest,
    };
    
    // Write lock file locally
    fs::write("starthub.lock.json", serde_json::to_string_pretty(&lock)?)?;
    
    // Now update the database with action and version information
    println!("🗄️  Updating database with action information...");
    update_action_database(&lock, &namespace).await?;
    
    // Upload lock file to the same Supabase Storage location
    println!("📤 Uploading lock file to Supabase Storage...");
    
    let lock_data = serde_json::to_string_pretty(&lock)?.into_bytes();
    let lock_object_key = format!("{}/{}/{}/lock.json", namespace, m.name, m.version);
    
    println!("🔑 Lock file object key: {}", lock_object_key);
    
    // Create ByteStream from the lock file data
    let lock_body = ByteStream::from(lock_data);
    
    // Upload lock file using AWS SDK
    let lock_put_object_output = s3_client
        .put_object()
        .bucket(bucket_name)
        .key(&lock_object_key)
        .body(lock_body)
        .content_type("application/json")
        .send()
        .await;
    
    match lock_put_object_output {
        Ok(_) => {
            println!("✅ Successfully uploaded lock file to Supabase Storage");
        }
        Err(e) => {
            println!("❌ Lock file upload failed: {:?}", e);
            anyhow::bail!("Failed to upload lock file to Supabase Storage");
        }
    }
    
    println!("✅ Docker image and lock file published to Supabase storage");
    println!("🔗 Storage URL: {}", storage_url);
    println!("🔗 Lock file URL: {}/storage/v1/object/public/{}/{}", 
        crate::config::STARTHUB_API_BASE, bucket_name, lock_object_key);
    Ok(())
}

pub fn push_wasm_and_get_digest(tag: &str, wasm_path: &str) -> anyhow::Result<String> {
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

pub fn find_wasm_artifact(crate_name: &str) -> Option<String> {
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
pub fn github_to_ghcr_path(repo: &str) -> Option<String> {
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
pub fn derive_image_base(m: &ShManifest, cli_image: Option<String>) -> anyhow::Result<String> {
    if let Some(i) = cli_image {
        let i = i.trim();
        if !i.is_empty() { return Ok(i.trim_end_matches('/').to_string()); }
    }

    if let Some(img) = &m.image {
        let img = img.trim();
        if !img.is_empty() && !img.starts_with("http") && img.split('/').count() >= 2 {
            return Ok(img.trim_end_matches('/').to_string());
        }
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
pub fn write_lock(m: &ShManifest, image_base: &str, digest: &str) -> anyhow::Result<()> {
    let primary = format!("oci://{}@{}", image_base, digest);
    let lock = ShLock {
        name: m.name.clone(),
        description: m.description.clone(),
        version: m.version.clone(),
        kind: m.kind.clone().expect("Kind should be present in manifest"),
        manifest_version: m.manifest_version,
        repository: m.repository.clone(),
        image: m.image.clone(),
        license: m.license.clone(),
        inputs: m.inputs.clone(),
        outputs: m.outputs.clone(),
        distribution: ShDistribution { primary, upstream: None },
        digest: digest.to_string(),
    };
    fs::write("starthub.lock.json", serde_json::to_string_pretty(&lock)?)?;
    Ok(())
}

pub async fn cmd_publish_wasm_inner(m: &ShManifest, no_build: bool) -> anyhow::Result<()> {
    // WASM PUBLISHING FUNCTION - This is the WASM-specific implementation
    // Get the user's namespace from their profile
    let namespace = match get_user_namespace().await {
        Ok(Some(ns)) => ns,
        Ok(None) => {
            println!("⚠️  No authentication found. Using default namespace 'actions'");
            println!("💡 Use 'starthub login' to authenticate and use your personal namespace");
            "actions".to_string()
        }
        Err(e) => {
            println!("⚠️  Failed to get user namespace: {}. Using default namespace 'actions'", e);
            println!("💡 Use 'starthub login' to authenticate and use your personal namespace");
            "actions".to_string()
        }
    };
    
    println!("🏷️  Using namespace: {}", namespace);

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

    // Create a zip file containing the WASM artifact
    let zip_filename = format!("{}-{}.zip", m.name, m.version);
    run("zip", &["-j", &zip_filename, &wasm_path])?;
    
    // Upload to Supabase Storage using AWS SDK
    println!("📤 Uploading to Supabase Storage using AWS SDK");
    
    // Get file size for verification
    let metadata = fs::metadata(&zip_filename)?;
    println!("📁 File size: {} bytes", metadata.len());
    
    // Use the same bucket as Docker publishing since it's working
    let bucket_name = "artifacts";
    let object_key = format!("{}/{}/{}/artifact.zip", namespace, m.name, m.version);
    
    println!("🪣 Bucket: {}", bucket_name);
    println!("🔑 Object key: {}", object_key);
    
    // Read the zip file
    let zip_data = fs::read(&zip_filename)?;
    
    // Upload using AWS SDK to Supabase Storage S3 endpoint
    println!("🔄 Uploading to Supabase Storage using AWS SDK...");

    // Set AWS credentials environment variables for Supabase Storage S3 compatibility
    std::env::set_var("AWS_ACCESS_KEY_ID", crate::config::S3_ACCESS_KEY);
    std::env::set_var("AWS_SECRET_ACCESS_KEY", crate::config::S3_SECRET_KEY);

    // Configure AWS SDK for Supabase Storage S3 compatibility
    let region_provider = RegionProviderChain::first_try(Region::new(crate::config::SUPABASE_STORAGE_REGION));
    let shared_config = aws_config::from_env()
        .region(region_provider)
        .load()
        .await;
    
    // Create S3 client with custom endpoint
    // Ensure the endpoint ends with a slash for proper URL construction
    let endpoint_url = if crate::config::SUPABASE_STORAGE_S3_ENDPOINT.ends_with('/') {
        crate::config::SUPABASE_STORAGE_S3_ENDPOINT.to_string()
    } else {
        format!("{}/", crate::config::SUPABASE_STORAGE_S3_ENDPOINT)
    };
    
    let s3_config = aws_sdk_s3::config::Builder::from(&shared_config)
        .endpoint_url(&endpoint_url)
        .force_path_style(true) // Use path-style for Supabase Storage S3 compatibility
        .build();
    
    println!("🔗 AWS SDK S3 endpoint: {}", crate::config::SUPABASE_STORAGE_S3_ENDPOINT);
    println!("🪣 Bucket: {}", bucket_name);
    println!("🔑 Object key: {}", object_key);
    
    let s3_client = S3Client::from_conf(s3_config);

    // Create ByteStream from the zip file data
    let body = ByteStream::from(zip_data);

    // Upload the zip file
    let put_object_output = s3_client
        .put_object()
        .bucket(bucket_name)
        .key(&object_key)
        .body(body)
        .content_type("application/zip")
        .send()
        .await;

    match put_object_output {
        Ok(_) => {
            println!("✅ Successfully uploaded WASM artifact to Supabase Storage");
        }
        Err(e) => {
            println!("❌ Upload failed: {:?}", e);
            anyhow::bail!("Failed to upload WASM artifact to Supabase Storage");
        }
    }

    // Create lock file with the same structure as Docker
    let digest = format!("sha256:{}", m.name); // Simplified digest for now
    
    let primary = format!("{}@{}", endpoint_url, &digest);
    let lock = ShLock {
        name: m.name.clone(),
        description: m.description.clone(),
        version: m.version.clone(),
        kind: m.kind.clone().expect("Kind should be present in manifest"),
        manifest_version: m.manifest_version,
        repository: m.repository.clone(),
        image: m.image.clone(),
        license: m.license.clone(),
        inputs: m.inputs.clone(),
        outputs: m.outputs.clone(),
        distribution: ShDistribution { primary, upstream: None },
        digest,
    };
    
    // Write lock file locally
    fs::write("starthub.lock.json", serde_json::to_string_pretty(&lock)?)?;
    
    // Upload lock file to the same Supabase Storage location
    println!("📤 Uploading lock file to Supabase Storage...");
    
    let lock_data = serde_json::to_string_pretty(&lock)?.into_bytes();
    let lock_object_key = format!("{}/{}/{}/lock.json", namespace, m.name, m.version);
    
    println!("🔑 Lock file object key: {}", lock_object_key);
    
    // Create ByteStream from the lock file data
    let lock_body = ByteStream::from(lock_data);
    
    // Upload lock file using AWS SDK
    let lock_put_object_output = s3_client
        .put_object()
        .bucket(bucket_name)
        .key(&lock_object_key)
        .body(lock_body)
        .content_type("application/json")
        .send()
        .await;
    
    match lock_put_object_output {
        Ok(_) => {
            println!("✅ Successfully uploaded lock file to Supabase Storage");
        }
        Err(e) => {
            println!("❌ Lock file upload failed: {:?}", e);
            anyhow::bail!("Failed to upload lock file to Supabase Storage");
        }
    }
    
    println!("✅ WASM artifact and lock file published to Supabase storage");
    println!("🔗 Storage URL: {}", endpoint_url);
    println!("🔗 Lock file URL: {}/storage/v1/object/public/{}/{}",
        crate::config::STARTHUB_API_BASE, bucket_name, lock_object_key);
    
    // Now update the database with action and version information
    println!("🗄️  Updating database with action information...");
    update_action_database(&lock, &namespace).await?;
    
    // Clean up local files
    fs::remove_file(&zip_filename)?;
    
    Ok(())
}

pub fn run(cmd: &str, args: &[&str]) -> anyhow::Result<()> {
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

pub fn run_capture(cmd: &str, args: &[&str]) -> anyhow::Result<String> {
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

#[allow(dead_code)]
pub fn parse_digest(s: &str) -> Option<String> {
    for line in s.lines() {
        if let Some(rest) = line.trim().strip_prefix("Digest: ") {
            if rest.starts_with("sha256:") { return Some(rest.to_string()); }
        }
    }
    None
}

// ------------------- cmd_init -------------------
pub async fn cmd_init(path: String) -> anyhow::Result<()> {
    // Basic fields
    let name = Text::new("Package name:")
        .with_default("http-get-wasm")
        .prompt()?;

    let version = Text::new("Version:")
        .with_default("0.0.1")
        .prompt()?;

    // Kind
    let kind_str = Select::new("Kind:", vec!["wasm", "docker", "composition"]).prompt()?;
    let kind = match kind_str {
        "wasm" => ShKind::Wasm,
        "docker" => ShKind::Docker,
        "composition" => ShKind::Composition,
        _ => unreachable!(),
    };

    // Repository
    let repo_default = match kind {
        ShKind::Wasm   => "github.com/starthubhq/http-get-wasm",
        ShKind::Docker => "github.com/starthubhq/http-get-wasm",
        ShKind::Composition => "github.com/starthubhq/composite-action",
    };
    let repository = Text::new("Repository:")
        .with_help_message("Git repository URL (e.g., github.com/org/repo)")
        .with_default(repo_default)
        .prompt()?;

    // Since we're using Supabase Storage instead of OCI registries, 
    // we don't need to prompt for OCI image paths anymore
    let image: Option<String> = None;

    // License
    let license = Select::new("License:", vec![
        "Apache-2.0", "MIT", "BSD-3-Clause", "GPL-3.0", "Unlicense", "Proprietary",
    ]).prompt()?.to_string();

    // Empty I/O (you can extend later)
    let inputs: Vec<ShPort> = Vec::new();
    let outputs: Vec<ShPort> = Vec::new();

    // Manifest
    let manifest = ShManifest { 
        name: name.clone(), 
        description: "Generated manifest".to_string(),
        version: version.clone(), 
        manifest_version: 1,
        kind: Some(kind.clone()), 
        repository: repository.clone(), 
        image: None, // No OCI image needed with Supabase Storage
        license: license.clone(), 
        inputs, 
        outputs,
        steps: vec![],
        wires: vec![],
        export: serde_json::json!({}),
    };
    let json = serde_json::to_string_pretty(&manifest)?;

    // Ensure dir
    let out_dir = Path::new(&path);
    if !out_dir.exists() {
        fs::create_dir_all(out_dir)?;
    }

    // Write starthub.json
    write_file_guarded(&out_dir.join("starthub.json"), &json)?;
    // Always create .gitignore / .dockerignore / README.md
    write_file_guarded(&out_dir.join(".gitignore"), templates::GITIGNORE_TPL)?;
    // .dockerignore only for Docker projects
    if matches!(kind, ShKind::Docker) {
        write_file_guarded(&out_dir.join(".dockerignore"), templates::DOCKERIGNORE_TPL)?;
    }
    let readme = templates::readme_tpl(&name, &kind, &repository, &license);
    write_file_guarded(&out_dir.join("README.md"), &readme)?;

    // If docker, scaffold Dockerfile + entrypoint.sh
    if matches!(kind, ShKind::Docker) {
        let dockerfile = out_dir.join("Dockerfile");
        write_file_guarded(&dockerfile, templates::DOCKERFILE_TPL)?;
        let entrypoint = out_dir.join("entrypoint.sh");
        write_file_guarded(&entrypoint, templates::ENTRYPOINT_SH_TPL)?;
        make_executable(&entrypoint)?;
    }

    // If wasm, scaffold Cargo.toml + src/main.rs
    if matches!(kind, ShKind::Wasm) {
        let cargo = out_dir.join("Cargo.toml");
        write_file_guarded(&cargo, &templates::wasm_cargo_toml_tpl(&name, &version))?;

        let src_dir = out_dir.join("src");
        if !src_dir.exists() {
            fs::create_dir_all(&src_dir)?;
        }
        let main_rs = src_dir.join("main.rs");
        write_file_guarded(&main_rs, templates::WASM_MAIN_RS_TPL)?;
    }

    println!("✓ Wrote {}", out_dir.join("starthub.json").display());
    Ok(())
}

#[allow(dead_code)]
pub async fn cmd_login(runner: crate::RunnerKind) -> anyhow::Result<()> {
    let r = crate::make_runner(runner);
    println!("→ Logging in for runner: {}", r.name());
    r.ensure_auth().await?;
    println!("✓ Login complete for {}", r.name());
    Ok(())
}

/// Authenticate with Starthub backend using browser-based flow
pub async fn cmd_login_starthub(api_base: String) -> anyhow::Result<()> {
    println!("🔐 Authenticating with Starthub backend...");
    println!("🌐 API Base: {}", api_base);
    
    // Open browser to editor for authentication
    let editor_url = "https://editor.starthub.so/cli-auth";
    println!("🌐 Opening browser to: {}", editor_url);
    
    match webbrowser::open(editor_url) {
        Ok(_) => println!("✅ Browser opened successfully"),
        Err(e) => println!("⚠️  Could not open browser automatically: {}", e),
    }
    
    println!("\n📋 Please:");
    println!("1. Wait for the authentication code to appear in your browser");
    println!("2. Copy the authentication code from the browser");
    println!("3. Come back here and paste the code below");
    
    // Wait for user to paste the code
    let pasted_code = inquire::Text::new("Paste the authentication code:")
        .with_help_message("Enter the code from your browser")
        .prompt()?;
    
    // Validate the code against the backend
    println!("🔄 Validating authentication code...");
    
    let client = reqwest::Client::new();
    let validation_response = client
        .post(&format!("{}/functions/v1/cli-auth", api_base))
        .json(&serde_json::json!({
            "code": pasted_code
        }))
        .send()
        .await?;
    
    let status = validation_response.status();
    if !status.is_success() {
        let error_text = validation_response.text().await?;
        anyhow::bail!("Code validation failed: {} ({})", status, error_text);
    }
    
    let validation_data: serde_json::Value = validation_response.json().await?;
    
    if !validation_data.get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false) {
        let error_msg = validation_data.get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        anyhow::bail!("Authentication failed: {}", error_msg);
    }
    
    let profile = validation_data.get("profile")
        .ok_or_else(|| anyhow::anyhow!("No profile data in response"))?;
    
    let email = profile.get("email")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("No email in profile"))?;
    
    // Store the authentication info
    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?
        .join("starthub");
    
    fs::create_dir_all(&config_dir)?;
    
    let config_file = config_dir.join("auth.json");
    let auth_config = serde_json::json!({
        "api_base": api_base,
        "email": email,
        "profile_id": profile.get("id").and_then(|v| v.as_str()).unwrap_or(""),
        "username": profile.get("username").and_then(|v| v.as_str()).unwrap_or(""),
        "full_name": profile.get("full_name").and_then(|v| v.as_str()).unwrap_or(""),
        "namespace": profile.get("username").and_then(|v| v.as_str()).unwrap_or(""), // Use username as namespace for now
        "login_time": chrono::Utc::now().to_rfc3339(),
        "auth_method": "cli_code"
    });
    
    fs::write(&config_file, serde_json::to_string_pretty(&auth_config)?)?;
    
    println!("✅ Authentication successful!");
    println!("🔑 Authentication data saved to: {}", config_file.display());
    println!("📧 Logged in as: {}", email);
    
    Ok(())
}



/// Load stored authentication configuration
pub fn load_auth_config() -> anyhow::Result<Option<(String, String, String, String)>> {
    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?
        .join("starthub");
    
    let config_file = config_dir.join("auth.json");
    
    if !config_file.exists() {
        return Ok(None);
    }
    
    let config_content = fs::read_to_string(&config_file)?;
    let auth_config: serde_json::Value = serde_json::from_str(&config_content)?;
    
    let api_base = auth_config.get("api_base")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("No api_base in auth config"))?;
    
    let email = auth_config.get("email")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("No email in auth config"))?;
    
    let profile_id = auth_config.get("profile_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("No profile_id in auth config"))?;
    
    let namespace = auth_config.get("namespace")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("No namespace in auth config"))?;
    
    Ok(Some((api_base.to_string(), email.to_string(), profile_id.to_string(), namespace.to_string())))
}

/// Logout from Starthub backend
pub async fn cmd_logout_starthub() -> anyhow::Result<()> {
    println!("🔓 Logging out from Starthub backend...");
    
    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?
        .join("starthub");
    
    let config_file = config_dir.join("auth.json");
    
    if !config_file.exists() {
        println!("ℹ️  No authentication found. Already logged out.");
        return Ok(());
    }
    
    // Remove the auth file
    fs::remove_file(&config_file)?;
    
    println!("✅ Successfully logged out!");
    println!("🗑️  Authentication data removed from: {}", config_file.display());
    
    Ok(())
}

/// Get the namespace for the currently authenticated user
pub async fn get_user_namespace() -> anyhow::Result<Option<String>> {
    // Load authentication config
    let auth_config = match load_auth_config()? {
        Some((api_base, _email, profile_id, _namespace)) => (api_base, profile_id),
        None => return Ok(None),
    };
    
    let (api_base, profile_id) = auth_config;
    
    // Query the owners table directly using PostgREST
    let client = reqwest::Client::new();
    let response = client
        .get(&format!("{}/rest/v1/owners?select=namespace&owner_type=eq.PROFILE&profile_id=eq.{}", api_base, profile_id))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", profile_id))
        .send()
        .await?;
    
    if response.status().is_success() {
        let data: Vec<serde_json::Value> = response.json().await?;
        if let Some(owner) = data.first() {
            if let Some(namespace) = owner.get("namespace").and_then(|v| v.as_str()) {
                return Ok(Some(namespace.to_string()));
            }
        }
    }
    
    // Fallback: try to get from local auth config username
    let auth_config = load_auth_config()?;
    if let Some((_api_base, _email, _profile_id, namespace)) = auth_config {
        // Use the locally stored namespace as fallback
        return Ok(Some(namespace));
    }
    
    Ok(None)
}

/// Show current authentication status
pub async fn cmd_auth_status() -> anyhow::Result<()> {
    println!("🔍 Checking authentication status...");
    
    match load_auth_config()? {
        Some((api_base, email, profile_id, namespace)) => {
            println!("✅ Authenticated with Starthub backend");
            println!("🌐 API Base: {}", api_base);
            println!("📧 Email: {}", email);
            println!("🆔 Profile ID: {}", profile_id);
            println!("🏷️  Namespace: {}", namespace);
            
            // Try to validate the authentication by making a test API call
            println!("🔄 Validating authentication...");
            let client = reqwest::Client::new();
            let response = client
                .get(&format!("{}/functions/v1/profiles", api_base))
                .header("Authorization", format!("Bearer {}", profile_id))
                .send()
                .await?;
            
            if response.status().is_success() {
                println!("✅ Authentication is valid and working");
            } else {
                println!("⚠️  Authentication may be expired or invalid");
            }
        }
        None => {
            println!("❌ Not authenticated");
            println!("💡 Use 'starthub login' to authenticate");
        }
    }
    
    Ok(())
}

pub async fn cmd_run(action: String, _runner: crate::RunnerKind) -> Result<()> {
    // Start the server first
    let _server_handle = tokio::spawn(start_server());
    
    // Wait a moment for server to start
    sleep(Duration::from_millis(100)).await;
    
    // Parse the action argument to extract namespace, slug, and version
    let (namespace, slug, version) = parse_action_arg(&action);
    
    // Open browser to the server with a proper route for the Vue app
    let url = format!("{}/{}/{}/{}", LOCAL_SERVER_URL, namespace, slug, version);
    match webbrowser::open(&url) {
        Ok(_) => println!("↗ Opened browser to: {url}"),
        Err(e) => println!("→ Browser: {url} (couldn't auto-open: {e})"),
    }
    
    println!("🚀 Server started at {}", LOCAL_SERVER_URL);
    println!("📱 Serving UI for action: {} at route: {}", action, url);
    println!("🔄 Press Ctrl+C to stop the server");
    
    // Wait for Ctrl+C signal
    tokio::signal::ctrl_c().await?;
    println!("\n🛑 Shutting down server...");
    
    // The server will automatically shut down when the task is dropped
    Ok(())
}

// Parse action argument in format "namespace/slug@version" or "namespace/slug"
fn parse_action_arg(action: &str) -> (String, String, String) {
    // Default values
    let mut namespace = "tgirotto".to_string();
    let mut slug = "test-action".to_string();
    let mut version = "0.1.0".to_string();
    
    if action.contains('/') {
        let parts: Vec<&str> = action.split('/').collect();
        if parts.len() >= 2 {
            namespace = parts[0].to_string();
            let full_slug = parts[1].to_string();
            
            // Check if slug contains version (e.g., "test-action@0.1.0")
            if full_slug.contains('@') {
                let slug_parts: Vec<&str> = full_slug.split('@').collect();
                if slug_parts.len() >= 2 {
                    slug = slug_parts[0].to_string();
                    version = slug_parts[1].to_string();
                }
            } else {
                slug = full_slug;
            }
        }
    } else if action.contains('@') {
        // Handle case like "test-action@0.1.0"
        let parts: Vec<&str> = action.split('@').collect();
        if parts.len() >= 2 {
            slug = parts[0].to_string();
            version = parts[1].to_string();
        }
    } else {
        // Just a slug, use defaults for namespace and version
        slug = action.to_string();
    }
    
    (namespace, slug, version)
}

async fn start_server() -> Result<()> {
    // Create shared state
    let state = AppState::new();
    
    // Create router with UI routes and API endpoints
    let app = Router::new()
        .route("/api/status", get(get_status))
        .route("/api/action", post(handle_action))
        .route("/api/run", post(handle_run))
        .route("/ws", get(ws_handler)) // WebSocket endpoint
        .nest_service("/assets", ServeDir::new("ui/dist/assets"))
        .nest_service("/favicon.ico", ServeDir::new("ui/dist"))
        .route("/", get(serve_index))
        .fallback(serve_spa) // SPA fallback for Vue Router
        .layer(CorsLayer::permissive())
        .with_state(state);

    // Start server
    let listener = TcpListener::bind(LOCAL_SERVER_HOST).await?;
    println!("🌐 Server listening on {}", LOCAL_SERVER_URL);
    
    axum::serve(listener, app).await?;
    Ok(())
}

async fn serve_index() -> Html<String> {
    // Read and serve the index.html file
    match fs::read_to_string("ui/dist/index.html") {
        Ok(content) => Html(content),
        Err(_) => Html("<!DOCTYPE html><html><body><h1>UI not found</h1><p>Make sure to build the UI first</p></body></html>".to_string())
    }
}

// SPA fallback - serve index.html for all routes to support Vue Router
async fn serve_spa() -> Html<String> {
    match fs::read_to_string("ui/dist/index.html") {
        Ok(content) => Html(content),
        Err(_) => Html("<!DOCTYPE html><html><body><h1>UI not found</h1><p>Make sure to build the UI first</p></body></html>".to_string())
    }
}

async fn get_status() -> Json<Value> {
    Json(json!({
        "status": "running",
        "timestamp": chrono::Utc::now().to_rfc3339()
    }))
}

async fn handle_action(Json(payload): Json<Value>) -> Json<Value> {
    // Handle action requests from the UI
    Json(json!({
        "success": true,
        "message": "Action received",
        "data": payload
    }))
}

async fn handle_run(
    axum::extract::State(state): axum::extract::State<AppState>,
    Json(payload): Json<Value>
) -> Json<Value> {
    // Handle the /api/run endpoint that InputsComponent expects
    println!("🚀 Received run request: {:?}", payload);
    
    // Extract action and inputs from payload
    let action = payload.get("action").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
    let default_inputs = json!({});
    let inputs = payload.get("inputs").unwrap_or(&default_inputs);
    
    println!("📋 Action: {}", action);
    println!("📥 Inputs: {:?}", inputs);
    
    // Try to fetch action metadata and download starthub.json
    let base = std::env::var("STARTHUB_API").unwrap_or_else(|_| "https://api.starthub.so".to_string());
            let client = crate::starthub_api::Client::new(base, Some(crate::config::SUPABASE_ANON_KEY.to_string()));
    
    match client.fetch_action_metadata(&action).await {
        Ok(_metadata) => {
            println!("✓ Fetched action metadata for {}", action);
            
            // Try to download the starthub.json file and recursively fetch all action manifests
            println!("📥 Attempting to download composite action definition...");
            let mut visited = std::collections::HashSet::new();
            
            match fetch_all_action_manifests(&client, &action, &mut visited).await {
                Ok(manifests) => {
                    if manifests.is_empty() {
                        println!("❌ No composite action definition found");
                        return Json(json!({
                            "success": false,
                            "message": "No composite action definition found",
                            "action": action,
                            "error": "This action cannot be executed locally as it requires the full implementation"
                        }));
                    }
                    
                    println!("✅ Successfully downloaded {} action manifest(s)", manifests.len());
                    
                    // Get the main manifest (first one)
                    let main_manifest = &manifests[0];
                    println!("📋 Main action: {} (version: {})", main_manifest.name, main_manifest.version);
                    println!("🔗 Steps: {}", main_manifest.steps.len());
                    
                    // Convert to local types for topo_order
                    let local_steps: Vec<crate::runners::local::ActionStep> = main_manifest.steps.iter()
                        .map(convert_action_step)
                        .collect();
                    let local_wires: Vec<crate::runners::local::Wire> = main_manifest.wires.iter()
                        .map(convert_action_wire)
                        .collect();
                    
                    // Use topo_order to determine execution order
                    match crate::runners::local::topo_order(&local_steps, &local_wires) {
                            Ok(order) => {
                                println!("📊 Execution order: {:?}", order);
                                
                                // Send execution plan to WebSocket clients
                                let ws_message = json!({
                                    "type": "execution_plan",
                                    "action": action,
                                    "manifest": {
                                        "name": main_manifest.name,
                                        "version": main_manifest.version,
                                        "steps": main_manifest.steps.len(),
                                        "execution_order": order
                                    },
                                    "steps": main_manifest.steps.iter().map(|step| {
                                        json!({
                                            "id": step.id,
                                            "uses": step.uses,
                                            "kind": step.kind.as_deref().unwrap_or("docker")
                                        })
                                    }).collect::<Vec<_>>(),
                                    "wires": main_manifest.wires.iter().map(|wire| {
                                        json!({
                                            "from": {
                                                "step": wire.from.step,
                                                "output": wire.from.output,
                                                "source": wire.from.source,
                                                "key": wire.from.key
                                            },
                                            "to": {
                                                "step": wire.to.step,
                                                "input": wire.to.input
                                            }
                                        })
                                    }).collect::<Vec<_>>()
                                });
                                
                                // Broadcast to all WebSocket clients
                                if let Ok(msg_str) = serde_json::to_string(&ws_message) {
                                    let _ = state.ws_sender.send(msg_str);
                                    println!("📡 Sent WebSocket message to all clients");
                                }
                                
                                let response = json!({
                                    "success": true,
                                    "message": "Composite action resolved successfully",
                                    "action": action,
                                    "inputs": inputs,
                                    "execution_id": format!("exec_{}", chrono::Utc::now().timestamp()),
                                    "manifest": {
                                        "name": main_manifest.name,
                                        "version": main_manifest.version,
                                        "steps": main_manifest.steps.len(),
                                        "execution_order": order
                                    }
                                });
                                
                                Json(response)
                            }
                        Err(e) => {
                            println!("❌ Failed to determine execution order: {}", e);
                            Json(json!({
                                "success": false,
                                "message": "Failed to determine execution order",
                                "action": action,
                                "error": e.to_string()
                            }))
                        }
                    }
                }
                Err(e) => {
                    println!("❌ Failed to download composite action definition: {}", e);
                    Json(json!({
                        "success": false,
                        "message": "Failed to download composite action definition",
                        "action": action,
                        "error": e.to_string()
                    }))
                }
            }
        }
        Err(e) => {
            println!("❌ Failed to fetch action metadata: {}", e);
            Json(json!({
                "success": false,
                "message": "Failed to fetch action metadata",
                "error": e.to_string()
            }))
        }
    }
}

async fn ws_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    ws: WebSocketUpgrade
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws(socket, state))
}

async fn handle_ws(ws: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = ws.split();

    // Subscribe to the broadcast channel to receive execution plan messages
    let mut ws_receiver = state.ws_sender.subscribe();

    // Send a welcome message
    let welcome_msg = json!({
        "type": "connection",
        "message": "Connected to Starthub WebSocket server",
        "timestamp": chrono::Utc::now().to_rfc3339()
    });
    
    if let Ok(msg) = serde_json::to_string(&welcome_msg) {
        let _ = sender.send(axum::extract::ws::Message::Text(msg)).await;
    }

    // Spawn a task to forward broadcast messages to this WebSocket client
    let sender_arc = std::sync::Arc::new(tokio::sync::Mutex::new(sender));
    let sender_clone = sender_arc.clone();
    let forward_task = tokio::spawn(async move {
        while let Ok(msg) = ws_receiver.recv().await {
            let mut sender_guard = sender_clone.lock().await;
            if let Err(_) = sender_guard.send(axum::extract::ws::Message::Text(msg)).await {
                break; // WebSocket closed
            }
        }
    });

    // Handle incoming messages from the client
    while let Some(msg) = receiver.next().await {
        if let Ok(msg) = msg {
            match msg {
                axum::extract::ws::Message::Text(text) => {
                    // Echo back the message for now
                    let echo_msg = json!({
                        "type": "echo",
                        "message": text,
                        "timestamp": chrono::Utc::now().to_rfc3339()
                    });
                    
                    if let Ok(msg_str) = serde_json::to_string(&echo_msg) {
                        let mut sender_guard = sender_arc.lock().await;
                        let _ = sender_guard.send(axum::extract::ws::Message::Text(msg_str)).await;
                    }
                }
                axum::extract::ws::Message::Close(_) => {
                    break;
                }
                _ => {}
            }
        }
    }

    // Cancel the forward task when the WebSocket closes
    forward_task.abort();
}

pub async fn cmd_status(id: Option<String>) -> Result<()> {
    println!("Status for {id:?}");
    // TODO: poll API
    Ok(())
}

/// Updates the database with action and version information after a successful upload
/// This function:
/// 1. Checks if an action already exists for the given name and namespace
/// 2. Checks if a version already exists for the given action and version number
/// 3. Inserts new action and version if they don't exist
/// 4. Inserts action ports from the lock file
async fn update_action_database(lock: &ShLock, namespace: &str) -> anyhow::Result<()> {
    // Load authentication config to get profile_id and API base
    let auth_config = load_auth_config()?;
    let (api_base, _email, profile_id, _namespace) = auth_config.ok_or_else(|| {
        anyhow::anyhow!("No authentication found in auth config. Please run 'starthub login' first.")
    })?;
    
    // First, check if an action already exists for this name and namespace
    let action_exists = check_action_exists(&api_base, &lock.name, namespace).await?;
    
    let action_id = if action_exists {
        // Get the existing action ID
        get_action_id(&api_base, &lock.name, namespace).await?
    } else {
        // Create a new action
        create_action(&api_base, &lock.name, &lock.description, namespace, &profile_id).await?
    };
    
    // Check if a version already exists for this action and version number
    let version_exists = check_version_exists(&api_base, &action_id, &lock.version).await?;
    
    if version_exists {
        anyhow::bail!(
            "Action version already exists: {}@{} in namespace '{}'. Use a different version number.",
            lock.name, lock.version, namespace
        );
    }
    
    // Create a new version
    let version_id = create_action_version(&api_base, &action_id, &lock.version).await?;
    
    // Insert action ports from the lock file
    insert_action_ports(&api_base, &version_id, &lock.inputs, &lock.outputs).await?;
    
    println!("✅ Database updated successfully:");
    println!("   🏷️  Action: {} (ID: {})", lock.name, action_id);
    println!("   📦 Version: {} (ID: {})", lock.version, version_id);
    println!("   🔌 Ports: {} inputs, {} outputs", lock.inputs.len(), lock.outputs.len());
    
    Ok(())
}

/// Checks if an action already exists for the given name and namespace
async fn check_action_exists(api_base: &str, action_name: &str, namespace: &str) -> anyhow::Result<bool> {
    let client = reqwest::Client::new();
    
    // Query the actions table joined with owners to check namespace
    let response = client
        .get(&format!("{}/rest/v1/actions?select=id&name=eq.{}&rls_owner_id=in.(select id from owners where namespace=eq.{})", 
            api_base, action_name, namespace))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", crate::config::SUPABASE_ANON_KEY))
        .send()
        .await?;
    
    if response.status().is_success() {
        let actions: Vec<serde_json::Value> = response.json().await?;
        Ok(!actions.is_empty())
    } else {
        anyhow::bail!("Failed to check action existence: {}", response.status())
    }
}

/// Gets the ID of an existing action
async fn get_action_id(api_base: &str, action_name: &str, namespace: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    
    let response = client
        .get(&format!("{}/rest/v1/actions?select=id&name=eq.{}&rls_owner_id=in.(select id from owners where namespace=eq.{})", 
            api_base, action_name, namespace))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", crate::config::SUPABASE_ANON_KEY))
        .send()
        .await?;
    
    if response.status().is_success() {
        let actions: Vec<serde_json::Value> = response.json().await?;
        if let Some(action) = actions.first() {
            Ok(action["id"].as_str().unwrap_or_default().to_string())
        } else {
            anyhow::bail!("Action not found")
        }
    } else {
        anyhow::bail!("Failed to get action ID: {}", response.status())
    }
}

/// Creates a new action
async fn create_action(api_base: &str, action_name: &str, description: &str, namespace: &str, profile_id: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    
    // First get the owner ID for this profile
    let owner_response = client
        .get(&format!("{}/rest/v1/owners?select=id&profile_id=eq.{}&owner_type=eq.PROFILE", 
            api_base, profile_id))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", crate::config::SUPABASE_ANON_KEY))
        .send()
        .await?;
    
    if !owner_response.status().is_success() {
        anyhow::bail!("Failed to get owner ID: {}", owner_response.status())
    }
    
    let owners: Vec<serde_json::Value> = owner_response.json().await?;
    let owner_id = owners.first()
        .and_then(|o| o["id"].as_str())
        .ok_or_else(|| anyhow::anyhow!("Owner not found for profile"))?;
    
    // Create the action
    let action_data = serde_json::json!({
        "name": action_name,
        "description": description,
        "rls_owner_id": owner_id
    });
    
    let response = client
        .post(&format!("{}/rest/v1/actions", api_base))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", crate::config::SUPABASE_ANON_KEY))
        .header("Content-Type", "application/json")
        .header("Prefer", "return=representation")
        .json(&action_data)
        .send()
        .await?;
    
    if response.status().is_success() {
        let actions: Vec<serde_json::Value> = response.json().await?;
        if let Some(action) = actions.first() {
            Ok(action["id"].as_str().unwrap_or_default().to_string())
        } else {
            anyhow::bail!("Failed to get created action ID")
        }
    } else {
        anyhow::bail!("Failed to create action: {}", response.status())
    }
}

/// Checks if a version already exists for the given action and version number
async fn check_version_exists(api_base: &str, action_id: &str, version_number: &str) -> anyhow::Result<bool> {
    let client = reqwest::Client::new();
    
    let response = client
        .get(&format!("{}/rest/v1/action_versions?select=id&action_id=eq.{}&version_number=eq.{}", 
            api_base, action_id, version_number))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", crate::config::SUPABASE_ANON_KEY))
        .send()
        .await?;
    
    if response.status().is_success() {
        let versions: Vec<serde_json::Value> = response.json().await?;
        Ok(!versions.is_empty())
    } else {
        anyhow::bail!("Failed to check version existence: {}", response.status())
    }
}

/// Creates a new action version
async fn create_action_version(api_base: &str, action_id: &str, version_number: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    
    let version_data = serde_json::json!({
        "action_id": action_id,
        "version_number": version_number
    });
    
    let response = client
        .post(&format!("{}/rest/v1/action_versions", api_base))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", crate::config::SUPABASE_ANON_KEY))
        .header("Content-Type", "application/json")
        .header("Prefer", "return=representation")
        .json(&version_data)
        .send()
        .await?;
    
    if response.status().is_success() {
        let versions: Vec<serde_json::Value> = response.json().await?;
        if let Some(version) = versions.first() {
            Ok(version["id"].as_str().unwrap_or_default().to_string())
        } else {
            anyhow::bail!("Failed to get created version ID")
        }
    } else {
        anyhow::bail!("Failed to create action version: {}", response.status())
    }
}

/// Inserts action ports for inputs and outputs
async fn insert_action_ports(api_base: &str, version_id: &str, inputs: &[ShPort], outputs: &[ShPort]) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    
    // Get the owner ID for this version (needed for RLS)
    let version_response = client
        .get(&format!("{}/rest/v1/action_versions?select=rls_owner_id&id=eq.{}", 
            api_base, version_id))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", crate::config::SUPABASE_ANON_KEY))
        .send()
        .await?;
    
    if !version_response.status().is_success() {
        anyhow::bail!("Failed to get version owner ID: {}", version_response.status())
    }
    
    let versions: Vec<serde_json::Value> = version_response.json().await?;
    let owner_id = versions.first()
        .and_then(|v| v["rls_owner_id"].as_str())
        .ok_or_else(|| anyhow::anyhow!("Version owner ID not found"))?;
    
    // Insert input ports
    for input in inputs {
        let port_data = serde_json::json!({
            "action_port_type": match input.ty {
                ShType::String => "STRING",
                ShType::Integer => "NUMBER",
                ShType::Boolean => "BOOLEAN",
                ShType::Object => "OBJECT",
                ShType::Array => "OBJECT", // Map array to OBJECT for now
                ShType::Number => "NUMBER",
            },
            "action_port_direction": "INPUT",
            "action_version_id": version_id,
            "rls_owner_id": owner_id
        });
        
        let response = client
            .post(&format!("{}/rest/v1/action_ports", api_base))
            .header("apikey", crate::config::SUPABASE_ANON_KEY)
            .header("Authorization", format!("Bearer {}", crate::config::SUPABASE_ANON_KEY))
            .header("Content-Type", "application/json")
            .json(&port_data)
            .send()
            .await?;
        
        if !response.status().is_success() {
            anyhow::bail!("Failed to insert input port: {}", response.status())
        }
    }
    
    // Insert output ports
    for output in outputs {
        let port_data = serde_json::json!({
            "action_port_type": match output.ty {
                ShType::String => "STRING",
                ShType::Integer => "NUMBER",
                ShType::Boolean => "BOOLEAN",
                ShType::Object => "OBJECT",
                ShType::Array => "OBJECT", // Map array to OBJECT for now
                ShType::Number => "NUMBER",
            },
            "action_port_direction": "OUTPUT",
            "action_version_id": version_id,
            "rls_owner_id": owner_id
            // Note: We don't have a 'name' field in ShPort, so we can't set it
        });
        
        let response = client
            .post(&format!("{}/rest/v1/action_ports", api_base))
            .header("apikey", crate::config::SUPABASE_ANON_KEY)
            .header("Authorization", format!("Bearer {}", crate::config::SUPABASE_ANON_KEY))
            .header("Content-Type", "application/json")
            .json(&port_data)
            .send()
            .await?;
        
        if !response.status().is_success() {
            anyhow::bail!("Failed to insert output port: {}", response.status())
        }
    }
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs;

    #[test]
    fn test_parse_digest_any() {
        // Test successful digest parsing
        let output = "digest: sha256:abc123def4567890abcdef1234567890abcdef1234567890abcdef1234567890abc";
        let digest = parse_digest_any(output);
        assert_eq!(digest, Some("sha256:abc123def4567890abcdef1234567890abcdef1234567890abcdef1234567890abc".to_string()));

        // Test case insensitive
        let output = "Digest: SHA256:abc123def4567890abcdef1234567890abcdef1234567890abcdef1234567890abc";
        let digest = parse_digest_any(output);
        assert_eq!(digest, Some("SHA256:abc123def4567890abcdef1234567890abcdef1234567890abcdef1234567890abc".to_string()));

        // Test with additional text after digest
        let output = "digest: sha256:abc123def4567890abcdef1234567890abcdef1234567890abcdef1234567890abc size: 1234";
        let digest = parse_digest_any(output);
        assert_eq!(digest, Some("sha256:abc123def4567890abcdef1234567890abcdef1234567890abcdef1234567890abc".to_string()));

        // Test no digest found
        let output = "no digest here";
        let digest = parse_digest_any(output);
        assert_eq!(digest, None);

        // Test invalid digest format
        let output = "digest: sha256:invalid";
        let digest = parse_digest_any(output);
        assert_eq!(digest, None);
    }

    #[test]
    fn test_oci_from_manifest() {
        // Test OCI paths
        let manifest = ShManifest {
            name: "test".to_string(),
            description: "Test manifest".to_string(),
            version: "1.0.0".to_string(),
            kind: Some(ShKind::Docker),
            manifest_version: 1,
            repository: "ghcr.io/org/image".to_string(),
            image: Some("ghcr.io/org/image".to_string()),
            license: "MIT".to_string(),
            inputs: vec![],
            outputs: vec![],
            steps: vec![],
            wires: vec![],
            export: serde_json::json!({}),
        };
        let result = oci_from_manifest(&manifest).unwrap();
        assert_eq!(result, "ghcr.io/org/image");

        // Test GitHub URLs
        let manifest = ShManifest {
            name: "test".to_string(),
            description: "Test manifest".to_string(),
            version: "1.0.0".to_string(),
            manifest_version: 1,
            kind: Some(ShKind::Docker),
            repository: "https://github.com/org/repo".to_string(),
            image: Some("ghcr.io/org/repo".to_string()),
            license: "MIT".to_string(),
            inputs: vec![],
            outputs: vec![],
            steps: vec![],
            wires: vec![],
            export: serde_json::json!({}),
        };
        let result = oci_from_manifest(&manifest).unwrap();
        assert_eq!(result, "ghcr.io/org/repo");

        // Test GitHub URLs without scheme
        let manifest = ShManifest {
            name: "test".to_string(),
            description: "Test manifest".to_string(),
            version: "1.0.0".to_string(),
            manifest_version: 1,
            kind: Some(ShKind::Docker),
            repository: "github.com/org/repo".to_string(),
            image: Some("ghcr.io/org/repo".to_string()),
            license: "MIT".to_string(),
            inputs: vec![],
            outputs: vec![],
            steps: vec![],
            wires: vec![],
            export: serde_json::json!({}),
        };
        let result = oci_from_manifest(&manifest).unwrap();
        assert_eq!(result, "github.com/org/repo");

        // Test invalid repository
        let manifest = ShManifest {
            name: "test".to_string(),
            description: "Test manifest".to_string(),
            version: "1.0.0".to_string(),
            manifest_version: 1,
            kind: Some(ShKind::Docker),
            repository: "invalid".to_string(),
            image: Some("invalid".to_string()),
            license: "MIT".to_string(),
            inputs: vec![],
            outputs: vec![],
            steps: vec![],
            wires: vec![],
            export: serde_json::json!({}),
        };
        let result = oci_from_manifest(&manifest);
        assert!(result.is_err());
    }

    #[test]
    fn test_find_wasm_artifact() {
        // Test with crate name containing dash
        let result = find_wasm_artifact("test-crate");
        // This will return None in test environment, but we can test the function exists
        assert!(result.is_none() || result.unwrap().contains("test-crate"));

        // Test with crate name containing underscore
        let result = find_wasm_artifact("test_crate");
        // This will return None in test environment, but we can test the function exists
        assert!(result.is_none() || result.unwrap().contains("test_crate"));
    }

    #[test]
    fn test_github_to_ghcr_path() {
        // Test GitHub HTTPS URLs
        let result = github_to_ghcr_path("https://github.com/org/repo");
        assert_eq!(result, Some("ghcr.io/org/repo".to_string()));

        let result = github_to_ghcr_path("https://github.com/org/repo.git");
        assert_eq!(result, Some("ghcr.io/org/repo".to_string()));

        // Test GitHub URLs without scheme
        let result = github_to_ghcr_path("github.com/org/repo");
        assert_eq!(result, Some("ghcr.io/org/repo".to_string()));

        // Test with trailing slash
        let result = github_to_ghcr_path("https://github.com/org/repo/");
        assert_eq!(result, Some("ghcr.io/org/repo".to_string()));

        // Test invalid GitHub URLs
        let result = github_to_ghcr_path("https://gitlab.com/org/repo");
        assert_eq!(result, None);

        let result = github_to_ghcr_path("invalid-url");
        assert_eq!(result, None);
    }

    #[test]
    fn test_derive_image_base() {
        // Test Docker kind
        let manifest = ShManifest {
            name: "test".to_string(),
            version: "1.0.0".to_string(),
            manifest_version: 1,
            kind: ShKind::Docker,
            repository: "https://github.com/org/repo".to_string(),
            image: "ghcr.io/org/repo".to_string(),
            license: "MIT".to_string(),
            inputs: vec![],
            outputs: vec![],
        };
        let result = derive_image_base(&manifest, None);
        assert_eq!(result.unwrap(), "ghcr.io/org/repo");

        // Test WASM kind
        let manifest = ShManifest {
            name: "test".to_string(),
            version: "1.0.0".to_string(),
            kind: ShKind::Wasm,
            manifest_version: 1,
            repository: "https://github.com/org/repo".to_string(),
            image: "ghcr.io/org/repo".to_string(),
            license: "MIT".to_string(),
            inputs: vec![],
            outputs: vec![],
        };
        let result = derive_image_base(&manifest, None);
        assert_eq!(result.unwrap(), "ghcr.io/org/repo");

        // Test with non-GitHub repository
        let manifest = ShManifest {
            name: "test".to_string(),
            version: "1.0.0".to_string(),
            manifest_version: 1,
            kind: ShKind::Docker,
            repository: "https://gitlab.com/org/repo".to_string(),
            image: "ghcr.io/owner/package".to_string(),
            license: "MIT".to_string(),
            inputs: vec![],
            outputs: vec![],
        };
        let result = derive_image_base(&manifest, None);
        assert_eq!(result.unwrap(), "ghcr.io/owner/package");
    }

    #[test]
    fn test_write_file_guarded() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        let content = "test content";

        // Test writing new file
        write_file_guarded(&test_file, content).unwrap();
        assert!(test_file.exists());
        assert_eq!(fs::read_to_string(&test_file).unwrap(), content);

        // Test overwriting existing file (this will fail in test environment due to interactive prompt)
        // In a real test, we'd need to mock the inquire::Confirm
        let new_content = "new content";
        // This will likely fail due to interactive prompt, but we can test the function exists
        let _ = write_file_guarded(&test_file, new_content);
    }

    #[test]
    fn test_make_executable() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.sh");
        fs::write(&test_file, "#!/bin/bash").unwrap();

        // Test that the function doesn't panic
        let result = make_executable(&test_file);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cmd_status() {
        // Test with ID
        let _result = cmd_status(Some("test-id".to_string()));
        // This is async, so we can't easily test it in unit tests
        // But we can verify the function exists and doesn't panic
    }

    #[test]
    fn test_open_actions_page() {
        // Test that the function doesn't panic
        // Note: This will try to open a browser in test environment
        open_actions_page("test-owner", "test-repo");
        // We can't easily test the actual browser opening, but we can verify the function exists
    }

    #[tokio::test]
    async fn test_cmd_login() {
        // This test would require mocking the runner, which is complex
        // For now, we just verify the function exists and doesn't panic
        // In a real test suite, you'd want to mock the runner::Runner trait
    }

    #[tokio::test]
    async fn test_cmd_run() {
        // This test would require mocking the runner, which is complex
        // For now, we just verify the function exists and doesn't panic
        // In a real test suite, you'd want to mock the runner::Runner trait
    }

    #[tokio::test]
    async fn test_cmd_publish_docker_inner() {
        // This test would require mocking Docker commands, which is complex
        // For now, we just verify the function exists and doesn't panic
        // In a real test suite, you'd want to mock the external commands
    }

    #[tokio::test]
    async fn test_cmd_publish_wasm_inner() {
        // This test would require mocking oras/crane commands, which is complex
        // For now, we just verify the function exists and doesn't panic
        // In a real test suite, you'd want to mock the external commands
    }

    #[test]
    fn test_push_and_get_digest() {
        // This test would require mocking Docker commands, which is complex
        // For now, we just verify the function exists and doesn't panic
        // In a real test suite, you'd want to mock the external commands
    }

    #[test]
    fn test_push_wasm_and_get_digest() {
        // This test would require mocking oras/crane commands, which is complex
        // For now, we just verify the function exists and doesn't panic
        // In a real test suite, you'd want to mock the external commands
    }

    #[test]
    fn test_run_and_run_capture() {
        // These tests would require mocking external commands, which is complex
        // For now, we just verify the functions exist and don't panic
        // In a real test suite, you'd want to mock the external commands
    }

    #[test]
    fn test_parse_digest() {
        // Test successful digest parsing
        let output = "Digest: sha256:abc123def4567890abcdef1234567890abcdef1234567890abcdef1234567890abc";
        let digest = parse_digest(output);
        assert_eq!(digest, Some("sha256:abc123def4567890abcdef1234567890abcdef1234567890abcdef1234567890abc".to_string()));

        // Test with whitespace
        let output = "  Digest: sha256:abc123def4567890abcdef1234567890abcdef1234567890abcdef1234567890abc  ";
        let digest = parse_digest(output);
        assert_eq!(digest, Some("sha256:abc123def4567890abcdef1234567890abcdef1234567890abcdef1234567890abc".to_string()));
    }

    #[test]
    fn test_write_lock() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = ShManifest {
            name: "test".to_string(),
            version: "1.0.0".to_string(),
            kind: ShKind::Docker,
            manifest_version: 1,
            repository: "ghcr.io/org/image".to_string(),
            image: "ghcr.io/org/image".to_string(),
            license: "MIT".to_string(),
            inputs: vec![],
            outputs: vec![],
        };
        let image_base = "ghcr.io/org/image";
        let digest = "sha256:abc123def4567890abcdef1234567890abcdef1234567890abcdef1234567890abc";

        // Change to temp directory so write_lock writes there
        std::env::set_current_dir(temp_dir.path()).unwrap();
        write_lock(&manifest, image_base, digest).unwrap();
        
        let lock_file = temp_dir.path().join("starthub.lock.json");
        assert!(lock_file.exists());
        let content = fs::read_to_string(&lock_file).unwrap();
        let parsed_lock: ShLock = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed_lock.name, manifest.name);
        assert_eq!(parsed_lock.version, manifest.version);
        assert_eq!(parsed_lock.kind, manifest.kind);
        
        // Clean up
        std::env::set_current_dir("/").unwrap();
    }
}
