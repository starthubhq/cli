// src/starthub_api.rs
use anyhow::{Result, Context};
use reqwest::header::{AUTHORIZATION, ACCEPT};
use serde::Deserialize;


use crate::config::{STARTHUB_API_BASE, STARTHUB_SUPABASE_ANON_KEY};

#[derive(Clone)]
pub struct Client {
    #[allow(dead_code)]
    base: String,
    token: Option<String>,
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
pub struct ActionMetadata {
    pub namespace: String,
    pub slug: String,
    pub version: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub inputs: Option<Vec<ActionInput>>,
    pub outputs: Option<Vec<ActionOutput>>,
    pub steps: Option<Vec<ActionStep>>,
    pub wires: Option<Vec<ActionWire>>,
    pub export: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct ActionInput {
    pub name: String,
    #[serde(rename = "type")]
    pub input_type: String,
    pub description: Option<String>,
    #[allow(dead_code)]
    pub required: Option<bool>,
    #[allow(dead_code)]
    pub default: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct ActionOutput {
    pub name: String,
    #[serde(rename = "type")]
    pub output_type: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ActionStep {
    pub id: String,
    pub kind: Option<String>,
    pub uses: String,
    pub with: Option<std::collections::HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
pub struct ActionWire {
    pub from: ActionWireFrom,
    pub to: ActionWireTo,
}

#[derive(Debug, Deserialize)]
pub struct ActionWireFrom {
    pub source: Option<String>,
    pub step: Option<String>,
    pub output: Option<String>,
    pub key: Option<String>,
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct ActionWireTo {
    pub step: String,
    pub input: String,
}

impl Client {
    pub fn new(base: impl Into<String>, token: Option<String>) -> Self {
        Self {
            base: base.into(),
            token,
            http: reqwest::Client::new(),
        }
    }

    #[allow(dead_code)]
    fn auth_header(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(t) = &self.token {
            req.header(AUTHORIZATION, format!("Bearer {t}"))
        } else { req }
    }

    /// Fetch action metadata from the actions edge function.
    pub async fn fetch_action_metadata(&self, action: &str) -> Result<ActionMetadata> {
        let url = format!("{}/functions/v1/actions", STARTHUB_API_BASE.trim_end_matches('/'));
        let mut req = self.http.get(&url)
            .header(ACCEPT, "application/json");
        
        // Use user's JWT token if available, otherwise fall back to anon key
        if let Some(user_token) = &self.token {
            req = req.header("Authorization", format!("Bearer {}", user_token));
        } else {
            req = req.header("Authorization", format!("Bearer {}", STARTHUB_SUPABASE_ANON_KEY));
        }
        
        req = req.query(&[("ref", action)]);
        
        let res = req.send().await?.error_for_status()?;
        
        let metadata: ActionMetadata = res.json().await.context("decoding action metadata json")?;
        Ok(metadata)
    }

    /// Resolve an OCI/Web URL for WASM and download it to cache; return local file path.
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
}

fn sanitize_ref_to_filename(r: &str) -> String {
    let mut s = r.replace("://", "_").replace('/', "_").replace('@', "_").replace(':', "_");
    if !s.ends_with(".wasm") { s.push_str(".wasm"); }
    s
}
