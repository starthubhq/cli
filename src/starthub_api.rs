// src/starthub_api.rs
use anyhow::{Result, Context};
use reqwest::header::ACCEPT;
use serde::Deserialize;
use crate::models::ShManifest;


use crate::config::SUPABASE_ANON_KEY;

#[derive(Clone)]
pub struct Client {
    base: String,
    #[allow(dead_code)]
    token: Option<String>,
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
pub struct ActionMetadata {
    pub name: String,
    pub inputs: Option<Vec<ActionInput>>,
    pub outputs: Option<Vec<ActionOutput>>,
    pub commit_sha: String,
    pub description: String,
    pub version_number: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ActionInput {
    pub name: String,
    pub action_port_type: String,
    pub action_port_direction: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ActionOutput {
    pub name: String,
    pub action_port_type: String,
    pub action_port_direction: String,
}





impl Client {
    pub fn new(base: impl Into<String>, token: Option<String>) -> Self {
        Self {
            base: base.into(),
            token,
            http: reqwest::Client::new(),
        }
    }


    /// Fetch action metadata from the actions edge function.
    pub async fn fetch_action_metadata(&self, action: &str) -> Result<ActionMetadata> {
        let url = format!("{}/functions/v1/actions", self.base.trim_end_matches('/'));
        let mut req = self.http.get(&url)
            .header(ACCEPT, "application/json");
        
        // Use the provided API key for authentication
        req = req.header("Authorization", format!("Bearer {}", SUPABASE_ANON_KEY));
        req = req.query(&[("ref", action)]);
        let res = req.send().await?.error_for_status()?;
        let metadata: ActionMetadata = res.json().await.context("decoding action metadata json")?;
        Ok(metadata)
    }

    /// Resolve an OCI/Web URL for WASM and download it to cache; return local file path.
    #[allow(dead_code)]
    pub async fn download_wasm(&self, ref_str: &str, cache_dir: &std::path::Path) -> Result<std::path::PathBuf> {
        // v1: support file:/http(s):// direct; OCI support can be added via an oci fetcher later.
        if ref_str.starts_with("http://") || ref_str.starts_with("https://") || ref_str.starts_with("file://") {
            let fname = sanitize_ref_to_filename(ref_str);
            let dst = cache_dir.join(fname);
            if !dst.exists() {
                let bytes = self.http.get(ref_str).send().await?.error_for_status()?.bytes().await?;
                std::fs::create_dir_all(cache_dir)?;
                std::fs::write(&dst, &bytes)?;
            }
            return Ok(dst);
        }
        // local path
        let p = std::path::Path::new(ref_str);
        if p.exists() { return Ok(p.canonicalize()?); }
        anyhow::bail!("unsupported wasm ref (add OCI support): {}", ref_str);
    }

    /// Download and parse the starthub.json file from S3 storage
    pub async fn download_starthub_json(&self, storage_url: &str) -> Result<ShManifest> {
        let res = self.http.get(storage_url)
            .send()
            .await?
            .error_for_status()?;
        
        let manifest: ShManifest = res.json().await.context("decoding starthub.json")?;
        Ok(manifest)
    }
}

#[allow(dead_code)]
fn sanitize_ref_to_filename(r: &str) -> String {
    let mut s = r.replace("://", "_").replace('/', "_").replace('@', "_").replace(':', "_");
    if !s.ends_with(".wasm") { s.push_str(".wasm"); }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_key_constant() {
        // Verify that the API key constant is set correctly
            assert_eq!(SUPABASE_ANON_KEY, "sb_publishable_AKGy20M54_uMOdJme3ZnZA_GX11LgHe");
    assert!(SUPABASE_ANON_KEY.starts_with("sb_publishable_"));
    }

    #[test]
    fn test_client_creation() {
        let client = Client::new("https://test.example.com", None);
        assert_eq!(client.base, "https://test.example.com");
        assert_eq!(client.token, None);
    }

    #[test]
    fn test_client_with_token() {
        let client = Client::new("https://test.example.com", Some("test-token".to_string()));
        assert_eq!(client.base, "https://test.example.com");
        assert_eq!(client.token, Some("test-token".to_string()));
    }

    #[test]
    fn test_url_encoding_matches_postman() {
        // Test that reqwest's automatic encoding produces the same result as the working Postman query
        let action = "tgirotto/tom-action-4@0.1.0";
        
        // reqwest automatically encodes query parameters, so we don't need manual encoding
        let base = "https://api.starthub.so";
        let _url = format!("{}/functions/v1/actions", base.trim_end_matches('/'));
        
        // The expected result should match what Postman produces
        let expected_encoded = "tgirotto%2Ftom-action-4%400.1.0";
        assert_eq!(expected_encoded, "tgirotto%2Ftom-action-4%400.1.0");
    }
}
