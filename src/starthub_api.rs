// src/starthub_api.rs
use anyhow::{Result, Context};
use reqwest::header::{AUTHORIZATION, ACCEPT, CONTENT_TYPE};
use serde::Deserialize;

use crate::runners::models::ActionPlan;

#[derive(Clone)]
pub struct Client {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl Client {
    pub fn new(base: impl Into<String>, token: Option<String>) -> Self {
        Self {
            base: base.into(),
            token,
            http: reqwest::Client::new(),
        }
    }

    fn auth_header(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(t) = &self.token {
            req.header(AUTHORIZATION, format!("Bearer {t}"))
        } else { req }
    }

    /// Fetch the resolved action plan (already expanded into atomic steps).
    pub async fn fetch_action_plan(&self, action: &str, env: Option<&str>) -> Result<ActionPlan> {
        // e.g. GET {base}/v1/actions/{action}/plan?env=…
        let url = format!("{}/v1/actions/{}/plan", self.base.trim_end_matches('/'), action);
        let mut req = self.http.get(&url).header(ACCEPT, "application/json");
        if let Some(e) = env { req = req.query(&[("env", e)]); }
        let req = self.auth_header(req);

        let res = req.send().await?.error_for_status()?;
        let plan: ActionPlan = res.json().await.context("decoding action plan json")?;
        Ok(plan)
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
