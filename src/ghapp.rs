use anyhow::{bail, Context, Result};
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{fs, path::PathBuf, time::{Duration, Instant}};
use tokio::time::sleep;

const UA: &str = "starthub-cli";

#[derive(Deserialize)]
pub struct DeviceStart {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: u32,
    interval: Option<u64>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct TokenResp {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: Option<u64>,
    pub refresh_token: Option<String>,
    pub refresh_token_expires_in: Option<u64>,
    pub scope: Option<String>,
}

#[derive(Deserialize)]
struct TokenPoll {
    access_token: Option<String>,
    token_type: Option<String>,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
    refresh_token_expires_in: Option<u64>,
    scope: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Deserialize)]
pub struct GhUser {
    #[allow(dead_code)]
    pub id: i64,
    pub login: String,
}

pub async fn device_login(client_id: &str) -> Result<TokenResp> {
    let client = reqwest::Client::new();

    let resp = client
        .post("https://github.com/login/device/code")
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "starthub-cli")
        .form(&[("client_id", client_id)])
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        // You'll see messages like: {"error":"incorrect_client_credentials"} etc.
        anyhow::bail!("device code start failed: {status} — {body}");
    }

    let start: DeviceStart = resp.json().await?;

    if let Some(url) = &start.verification_uri_complete {
        println!("Open this URL to authorize:\n{url}\n");
    } else {
        println!("Go to {}\nEnter code: {}\n", start.verification_uri, start.user_code);
    }

    // 2) Poll for token
    let interval = Duration::from_secs(start.interval.unwrap_or(5).max(5));
    let deadline = Instant::now() + Duration::from_secs(start.expires_in as u64);

    loop {
        if Instant::now() >= deadline {
            bail!("authorization timed out");
        }

        let resp: TokenPoll = client
            .post("https://github.com/login/oauth/access_token")
            .header(ACCEPT, "application/json")
            .form(&[
                ("client_id", client_id.to_string()),
                ("device_code", start.device_code.clone()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code".to_string()),
            ])
            .send().await?
            .error_for_status()?
            .json().await?;

        match (resp.access_token, resp.error.as_deref()) {
            (Some(at), _) => {
                return Ok(TokenResp {
                    access_token: at,
                    token_type: resp.token_type.unwrap_or_else(|| "bearer".into()),
                    expires_in: resp.expires_in,
                    refresh_token: resp.refresh_token,
                    refresh_token_expires_in: resp.refresh_token_expires_in,
                    scope: resp.scope,
                });
            }
            (None, Some("authorization_pending")) => sleep(interval).await,
            (None, Some("slow_down")) => sleep(interval + Duration::from_secs(5)).await,
            (None, Some("access_denied")) => bail!("authorization denied"),
            (None, Some(e)) => bail!(resp.error_description.unwrap_or_else(|| e.to_string())),
            _ => sleep(interval).await,
        }
    }
}

pub async fn get_user(access_token: &str) -> Result<GhUser> {
    let client = reqwest::Client::new();
    let me: GhUser = client
        .get("https://api.github.com/user")
        .header(ACCEPT, "application/vnd.github+json")
        .header(USER_AGENT, UA)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .send().await?
        .error_for_status()?
        .json().await?;
    Ok(me)
}

#[derive(Deserialize)]
struct InstallationsPage {
    installations: Vec<Installation>,
}

#[derive(Deserialize, Clone)]
pub struct Installation {
    pub id: i64,
    pub app_id: i64,
    pub account: Account,
}

#[derive(Deserialize, Clone)]
pub struct Account {
    #[allow(dead_code)]
    pub id: i64,
    pub login: String,
    #[serde(rename = "type")]
    pub account_type: String, // "User" | "Organization"
}

pub async fn find_installation_for_app(access_token: &str, app_id: i64) -> Result<Option<Installation>> {
    let client = reqwest::Client::new();
    let page: InstallationsPage = client
        .get("https://api.github.com/user/installations?per_page=100")
        .header(ACCEPT, "application/vnd.github+json")
        .header(USER_AGENT, UA)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .send().await?
        .error_for_status()?
        .json().await?;

    Ok(page.installations.into_iter().find(|i| i.app_id == app_id))
}

pub async fn wait_for_installation(access_token: &str, app_id: i64, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    loop {
        if let Some(inst) = find_installation_for_app(access_token, app_id).await? {
            println!("✓ App installed for {} ({}) [installation {}]", inst.account.login, inst.account.account_type, inst.id);
            return Ok(());
        }
        if Instant::now().duration_since(start) > timeout {
            bail!("timed out waiting for installation");
        }
        println!("… waiting for installation to appear");
        sleep(Duration::from_secs(3)).await;
    }
}

fn creds_path(provider: &str) -> Result<PathBuf> {
    let base = std::env::var("STARTHUB_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|_| {
            let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"))?;
            Ok::<_, anyhow::Error>(PathBuf::from(home).join(".starthub"))
        })?;
    Ok(base.join("creds").join(format!("{provider}.json")))
}#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StoredCredentials {
    pub access_token: String,
    pub token_type: String,
    pub expires_at_epoch: Option<u64>,                // now + expires_in (secs)
    pub refresh_token: Option<String>,
    pub refresh_token_expires_at_epoch: Option<u64>,  // now + refresh_expires_in
}

// ADD THIS impl (below the struct)
impl StoredCredentials {
    /// Treat token as expired if it has no expiry or will expire within 2 minutes.
    pub fn is_expired(&self) -> bool {
        match self.expires_at_epoch {
            None => false, // some GitHub tokens won't include expires_in – treat as non-expiring
            Some(t) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                now + 120 >= t
            }
        }
    }

}

pub fn save_token_for(provider: &str, token: &TokenResp) -> Result<()> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let creds = StoredCredentials {
        access_token: token.access_token.clone(),
        token_type: token.token_type.clone(),
        expires_at_epoch: token.expires_in.map(|e| now + e),
        refresh_token: token.refresh_token.clone(),
        refresh_token_expires_at_epoch: token.refresh_token_expires_in.map(|e| now + e),
    };
    let path = creds_path(provider)?;
    if let Some(dir) = path.parent() { fs::create_dir_all(dir)?; }
    fs::write(&path, serde_json::to_vec_pretty(&creds)?)?;
    Ok(())
}

pub fn load_token_for(provider: &str) -> Result<StoredCredentials> {
    let path = creds_path(provider)?;
    let data = fs::read(&path).with_context(|| format!("no credentials at {}", path.display()))?;
    Ok(serde_json::from_slice(&data)?)
}

// Returned subset from GitHub
#[derive(Deserialize, Debug)]
pub struct RepoInfo {
    pub name: String,
    pub full_name: String,     // e.g. "octocat/chirpstack-starthub"
    pub html_url: String,
    pub owner: RepoOwner,
}
#[derive(Deserialize, Debug)]
pub struct RepoOwner { pub login: String }

/// Create a personal repo for the authenticated user from a template repository.
/// Docs: POST /repos/{template_owner}/{template_repo}/generate
pub async fn create_repo_from_template_personal(
    user_token: &str,
    template_owner: &str,
    template_repo: &str,
    new_name: &str,
    private_: bool,
    description: Option<&str>,
    include_all_branches: bool,
    owner_login: Option<&str>,      // pass Some(login) explicitly (safe for personal)
) -> Result<RepoInfo> {
    let client = reqwest::Client::new();
    let url = format!(
        "https://api.github.com/repos/{}/{}/generate",
        template_owner, template_repo
    );

    // Build body selectively
    let mut map = serde_json::Map::new();
    map.insert("name".into(), json!(new_name));
    map.insert("private".into(), json!(private_));
    map.insert("include_all_branches".into(), json!(include_all_branches));
    if let Some(desc) = description { map.insert("description".into(), json!(desc)); }
    if let Some(owner) = owner_login { map.insert("owner".into(), json!(owner)); }
    let body = Value::Object(map);

    let resp = client.post(url)
        .header(ACCEPT, "application/vnd.github+json")
        .header(USER_AGENT, UA)
        .header(AUTHORIZATION, format!("Bearer {user_token}"))
        .header("X-GitHub-Api-Version", "2022-11-28")
        .json(&body)
        .send().await?;

    if resp.status().is_success() || resp.status().as_u16() == 201 {
        let repo: RepoInfo = resp.json().await?;
        return Ok(repo);
    }

    // Helpful error surface
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status.as_u16() == 422 && text.contains("must be a template repository") {
        bail!("Template repo is not marked as a template. Enable 'Template repository' on {}/{}.", template_owner, template_repo);
    }
    if status.as_u16() == 422 && text.contains("name already exists") {
        bail!("A repository named '{new_name}' already exists under your account.");
    }
    bail!("Template generate failed: {status} — {text}");
}

pub async fn create_user_repo(
    user_token: &str,
    name: &str,
    private_: bool,
    description: Option<&str>,
) -> Result<RepoInfo> {
    let client = reqwest::Client::new();
    let mut body = serde_json::Map::new();
    body.insert("name".into(), json!(name));
    body.insert("private".into(), json!(private_));
    if let Some(d) = description { body.insert("description".into(), json!(d)); }

    let resp = client.post("https://api.github.com/user/repos")
        .header(ACCEPT, "application/vnd.github+json")
        .header(USER_AGENT, UA)
        .header(AUTHORIZATION, format!("Bearer {user_token}"))
        .json(&body)
        .send().await?;

    if resp.status().is_success() {
        Ok(resp.json().await?)
    } else {
        let s = resp.status();
        let t = resp.text().await.unwrap_or_default();
        bail!("create repo failed: {s} — {t}");
    }
}

