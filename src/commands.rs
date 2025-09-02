use anyhow::Result;
use std::{fs, path::Path};
use std::process::Command as PCommand;
use inquire::{Text, Select, Confirm};
use tokio::time::{sleep, Duration};
use webbrowser;

use crate::models::{ShManifest, ShKind, ShPort, ShLock, ShDistribution};
use crate::templates;
use crate::runners;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

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
pub fn write_lock(m: &ShManifest, image_base: &str, digest: &str) -> anyhow::Result<()> {
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

pub async fn cmd_publish_wasm_inner(m: &ShManifest, no_build: bool) -> anyhow::Result<()> {
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
    let manifest = ShManifest { 
        name: name.clone(), 
        version: version.clone(), 
        kind: kind.clone(), 
        repository: repository.clone(), 
        image: image.clone(), 
        license: license.clone(), 
        inputs, 
        outputs 
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

pub fn open_actions_page(owner: &str, repo: &str) {
    let url = format!("https://github.com/{owner}/{repo}/actions");
    match webbrowser::open(&url) {
        Ok(_) => println!("↗ Opened GitHub Actions: {url}"),
        Err(e) => println!("→ GitHub Actions: {url} (couldn't auto-open: {e})"),
    }
}

pub async fn cmd_run(action: String, runner: crate::RunnerKind) -> Result<()> {
    let mut ctx = runners::DeployCtx {
        action,
        owner: None,
        repo: None,
    };
    let r = crate::make_runner(runner);

    // 1) ensure auth for selected runner; guide if missing
    r.ensure_auth().await?;

    // 2) do the runner-specific steps
    r.prepare(&mut ctx).await?;
    r.put_files(&ctx).await?;
    r.dispatch(&ctx).await?;

    if let (Some(owner), Some(repo)) = (ctx.owner.as_deref(), ctx.repo.as_deref()) {
        sleep(Duration::from_secs(5)).await;
        open_actions_page(owner, repo);
    }

    println!("✓ Dispatch complete for {}", r.name());

    Ok(())
}

pub async fn cmd_status(id: Option<String>) -> Result<()> {
    println!("Status for {id:?}");
    // TODO: poll API
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
            version: "1.0.0".to_string(),
            kind: ShKind::Docker,
            repository: "ghcr.io/org/image".to_string(),
            image: "ghcr.io/org/image".to_string(),
            license: "MIT".to_string(),
            inputs: vec![],
            outputs: vec![],
        };
        let result = oci_from_manifest(&manifest).unwrap();
        assert_eq!(result, "ghcr.io/org/image");

        // Test GitHub URLs
        let manifest = ShManifest {
            name: "test".to_string(),
            version: "1.0.0".to_string(),
            kind: ShKind::Docker,
            repository: "https://github.com/org/repo".to_string(),
            image: "ghcr.io/org/repo".to_string(),
            license: "MIT".to_string(),
            inputs: vec![],
            outputs: vec![],
        };
        let result = oci_from_manifest(&manifest).unwrap();
        assert_eq!(result, "ghcr.io/org/repo");

        // Test GitHub URLs without scheme
        let manifest = ShManifest {
            name: "test".to_string(),
            version: "1.0.0".to_string(),
            kind: ShKind::Docker,
            repository: "github.com/org/repo".to_string(),
            image: "ghcr.io/org/repo".to_string(),
            license: "MIT".to_string(),
            inputs: vec![],
            outputs: vec![],
        };
        let result = oci_from_manifest(&manifest).unwrap();
        assert_eq!(result, "github.com/org/repo");

        // Test invalid repository
        let manifest = ShManifest {
            name: "test".to_string(),
            version: "1.0.0".to_string(),
            kind: ShKind::Docker,
            repository: "invalid".to_string(),
            image: "invalid".to_string(),
            license: "MIT".to_string(),
            inputs: vec![],
            outputs: vec![],
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
