use anyhow::{bail, Result};
use super::{Runner, DeployCtx};
use crate::{ghapp, config};
use tokio::process::Command;
use tempfile::tempdir;
use flate2::read::GzDecoder;
use tar::Archive;
use std::{fs, io, path::{Path, PathBuf}, time::Duration};
// + encryption crates
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use sodiumoxide::init as sodium_init;
use sodiumoxide::crypto::{sealedbox, box_}; // <-- note: both modules imported

fn sanitize_secret_name(s: &str) -> String {
    // GitHub requires [A-Z0-9_]; normalize user-provided keys
    let up = s.trim().to_uppercase();
    up.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn sealed_box_encrypt_b64(pubkey_b64: &str, plaintext: &[u8]) -> Result<String> {
    sodium_init().map_err(|_| anyhow::anyhow!("libsodium init failed"))?;

    let pk_bytes = B64.decode(pubkey_b64)?;
    // sealedbox expects a box_::PublicKey
    let pk = box_::PublicKey::from_slice(&pk_bytes)
        .ok_or_else(|| anyhow::anyhow!("invalid public key length"))?;

    let cipher = sealedbox::seal(plaintext, &pk);
    Ok(B64.encode(cipher))
}

pub struct GithubRunner;

#[async_trait::async_trait]
impl Runner for GithubRunner {
    fn name(&self) -> &'static str { "github" }

     async fn ensure_auth(&self) -> Result<()> {
        // (unchanged) make sure user token + app installation exist
        let mut need = true;
        if let Ok(creds) = ghapp::load_token_for("github") {
            if !creds.is_expired() { need = false; }
        }
        if need {
            let token = ghapp::device_login(config::GH_CLIENT_ID).await?;
            let me = ghapp::get_user(&token.access_token).await?;
            println!("✓ Authorized as {}", me.login);
            ghapp::save_token_for("github", &token)?;
        }

        let tok = ghapp::load_token_for("github")?;
        if ghapp::find_installation_for_app(&tok.access_token, config::GH_APP_ID).await?.is_none() {
            let install_url = format!("https://github.com/apps/{}/installations/new", config::GH_APP_SLUG);
            println!("→ App not installed. Opening install page…\n{install_url}\n");
            let _ = webbrowser::open(&install_url);
            ghapp::wait_for_installation(&tok.access_token, config::GH_APP_ID, Duration::from_secs(300)).await?;
        }
        Ok(())
    }

    async fn prepare(&self, ctx: &mut DeployCtx) -> Result<()> {
        // Map package → template repo
        let (tpl_owner, tpl_repo, refspec) = match ctx.action.as_str() {
            "chirpstack" => ("starthubhq", "chirpstack", "refs/heads/main"),
            other => { anyhow::bail!("unknown package '{other}'"); }
        };
        let repo_name = format!("{}-starthub", ctx.action);

        let creds = ghapp::load_token_for("github")?;
        let me = ghapp::get_user(&creds.access_token).await?;
        println!("→ Creating personal repo '{}/{}' from {}/{} template…",
                me.login, repo_name, tpl_owner, tpl_repo);

        // 1) try native "generate from template"
        if let Ok(created) = ghapp::create_repo_from_template_personal(
            &creds.access_token,
            tpl_owner, tpl_repo,
            &repo_name,
            true, Some("Provisioned by Starthub"),
            false, Some(&me.login),
        ).await {
            println!("✓ Created via template: {} ({})", created.full_name, created.html_url);
            ctx.owner = Some(created.owner.login);
            ctx.repo = Some(created.name);
            return Ok(());
        } else {
            println!("↪︎ Falling back to clone+push (template not accessible by App)");
        }

        // 2) fallback: create empty repo
        let created = ghapp::create_user_repo(
            &creds.access_token,
            &repo_name,
            true,
            Some("Provisioned by Starthub"),
        ).await?;
        println!("✓ Created empty repo: {} ({})", created.full_name, created.html_url);

        // 3) download template tarball (public) and push
        let work = tempdir()?;
        let extract_root = work.path().join("src");
        let checkout = work.path().join("repo");
        fs::create_dir_all(&extract_root)?;
        fs::create_dir_all(&checkout)?;

        let tar_url = format!("https://codeload.github.com/{}/{}/tar.gz/{}",
                              tpl_owner, tpl_repo, refspec);
        let unpacked = download_tarball(&tar_url, &extract_root).await?;

        // copy all files into target checkout (strip top-level dir)
        copy_dir_all(&unpacked, &checkout)?;

        // init git and push (uses token in URL; OK for now, but keep it short-lived)
        let remote = format!(
            "https://x-access-token:{}@github.com/{}/{}.git",
            creds.access_token, created.owner.login, created.name
        );

        run_git(&checkout, &["init"]).await?;
        run_git(&checkout, &["checkout", "-b", "main"]).await?;
        run_git(&checkout, &["add", "."]).await?;
        run_git(&checkout, &["-c","user.name=Starthub","-c","user.email=bot@starthub.so","commit","-m","chore: bootstrap from template"]).await?;
        run_git(&checkout, &["remote", "add", "origin", &remote]).await?;
        run_git(&checkout, &["push", "-u", "origin", "main"]).await?;

        println!("✓ Pushed template contents to {}", created.full_name);
        ctx.owner = Some(created.owner.login);
        ctx.repo = Some(created.name);
        Ok(())
    }


    async fn put_files(&self, _ctx: &DeployCtx) -> Result<()> {
        // Not needed if the template already has the workflow & files.
        Ok(())
    }

    async fn set_secrets(&self, ctx: &DeployCtx) -> Result<()> {
        if ctx.secrets.is_empty() {
            println!("→ No -e secrets provided; skipping secret creation.");
            return Ok(());
        }
        let owner = ctx.owner.as_deref().ok_or_else(|| anyhow::anyhow!("owner missing; call prepare() first"))?;
        let repo  = ctx.repo.as_deref().ok_or_else(|| anyhow::anyhow!("repo missing; call prepare() first"))?;

        let creds = ghapp::load_token_for("github")?;
        let pk = ghapp::get_repo_public_key(&creds.access_token, owner, repo).await?;

        for (k, v) in &ctx.secrets {
            let name = sanitize_secret_name(k);
            let enc  = sealed_box_encrypt_b64(&pk.key, v.as_bytes())?;
            ghapp::put_repo_secret(&creds.access_token, owner, repo, &name, &pk.key_id, &enc).await?;
            println!("✓ Secret set: {}", name);
        }
        Ok(())
    }

    async fn dispatch(&self, ctx: &DeployCtx) -> Result<()> {
        // Optional: trigger a workflow if the template repo includes one
        println!("→ (dispatch) would trigger workflow in {}/{}",
                 ctx.owner.as_deref().unwrap_or("?"),
                 ctx.repo.as_deref().unwrap_or("?"));
        Ok(())
    }
}

async fn download_tarball(url: &str, dest: &Path) -> Result<PathBuf> {
    let bytes = reqwest::get(url).await?.error_for_status()?.bytes().await?;
    let gz = GzDecoder::new(std::io::Cursor::new(bytes));
    let mut ar = Archive::new(gz);
    ar.unpack(dest)?; // creates top-level dir like "<repo>-<sha>/"
    // find the single top-level dir
    let mut entries = fs::read_dir(dest)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false));
    let dir = entries.next().ok_or_else(|| anyhow::anyhow!("archive empty"))?;
    Ok(dir.path())
}

fn copy_dir_all(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else if ty.is_file() {
            fs::create_dir_all(to.parent().unwrap())?;
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

async fn run_git(dir: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git").args(args).current_dir(dir).status().await?;
    if !status.success() {
        bail!("git {:?} failed with status {:?}", args, status);
    }
    Ok(())
}
