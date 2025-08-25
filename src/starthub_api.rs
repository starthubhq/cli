use anyhow::Result;
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Deserialize)]
pub struct VersionRow {
    pub id: String,
    pub version: Option<String>,
    pub commit: Option<String>,
    pub created_at: Option<String>,
    pub configuration: Option<serde_json::Value>, // { inputs: {...}, ... }
}

#[derive(Debug, Deserialize)]
pub struct PackageRow {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub repository: String,          // ← we’ll use this as source
    pub namespace: Option<String>,
    pub versions: Option<VersionRow>,
    // package_types is returned too, add if you need it
}

fn fn_base() -> String {
    // e.g. "https://abcd1234.functions.supabase.co"
    // std::env::var("STARTHUB_FN_BASE")
    //     .expect("STARTHUB_FN_BASE must be set to your Supabase Functions base URL")
    return "https://api.starthub.so".to_string()
}

fn fn_token() -> String {
    // Any JWT your function accepts (service token, or a dedicated machine/user token).
    // std::env::var("STARTHUB_FN_TOKEN")
    //     .expect("STARTHUB_FN_TOKEN must be set (Authorization: Bearer <token>)")

    return "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJzdXBhYmFzZSIsInJlZiI6InNtbHRuanJyemttYXp2YnJxYmtxIiwicm9sZSI6ImFub24iLCJpYXQiOjE3MzY3ODk1NzcsImV4cCI6MjA1MjM2NTU3N30.2iz-ErTvlZ_o8rvYfFWWhlbo6RRTE0FWFlk7vQQkETg".to_string()
}

pub async fn get_package_by_name(name: &str) -> Result<PackageRow> {
    let url = format!("{}/packages/{}", fn_base(), name);

    let client = reqwest::Client::builder()
        .user_agent("starthub-cli")
        .timeout(Duration::from_secs(15))
        .build()?;

    // Some setups also require the anon key in `apikey` — add it if your function checks it:
    // let anon = std::env::var("SUPABASE_ANON_KEY").ok();

    let mut req = client.get(url)
        .bearer_auth(fn_token());

    // if let Some(key) = anon.as_ref() {
    //     req = req.header("apikey", key);
    // }

    let resp = req.send().await?;

    if resp.status().is_success() {
        let pkg: PackageRow = resp.json().await?;
        Ok(pkg)
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(anyhow::anyhow!("edge function error {}: {}", status, body))
    }
}
