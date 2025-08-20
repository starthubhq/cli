use anyhow::{anyhow, bail, Context, Result};
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::PathBuf, time::{Duration, Instant}};
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
    total_count: u32,
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

// ---------- simple file storage (no extra deps) ----------

#[derive(Serialize, Deserialize)]
pub struct StoredCredentials {
    pub access_token: String,
    pub token_type: String,
    pub expires_at_epoch: Option<u64>,       // now + expires_in (secs)
    pub refresh_token: Option<String>,
    pub refresh_token_expires_at_epoch: Option<u64>,
}

pub fn save_token(token: &TokenResp) -> Result<()> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let creds = StoredCredentials {
        access_token: token.access_token.clone(),
        token_type: token.token_type.clone(),
        expires_at_epoch: token.expires_in.map(|e| now + e),
        refresh_token: token.refresh_token.clone(),
        refresh_token_expires_at_epoch: token.refresh_token_expires_in.map(|e| now + e),
    };
    let path = creds_path()?;
    if let Some(dir) = path.parent() { fs::create_dir_all(dir)?; }
    let json = serde_json::to_vec_pretty(&creds)?;
    let mut f = fs::File::create(&path)?;
    f.write_all(&json)?;
    Ok(())
}

pub fn load_token() -> Result<StoredCredentials> {
    let path = creds_path()?;
    let data = fs::read(&path).with_context(|| format!("no credentials at {}", path.display()))?;
    Ok(serde_json::from_slice(&data)?)
}

fn creds_path() -> Result<PathBuf> {
    // Very small cross-platform shim; good enough to start.
    if let Ok(dir) = std::env::var("STARTHUB_CONFIG_DIR") {
        return Ok(PathBuf::from(dir).join("credentials.json"));
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE")) // Windows
        .context("HOME/USERPROFILE not set")?;
    Ok(PathBuf::from(home).join(".starthub").join("credentials.json"))
}
