// src/starthub_api.rs
use anyhow::Result;
use reqwest::header::AUTHORIZATION;

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

    /// Fetch a starthub.json file from Supabase storage bucket
    pub async fn fetch_starthub_json(&self, composition_id: &str) -> Result<String> {
        let url = format!("{}/storage/v1/object/public/compositions/{}/starthub.json", self.base, composition_id);
        
        let mut req = self.http.get(&url);
        req = self.auth_header(req);
        
        let resp = req.send().await?.error_for_status()?;
        let content = resp.text().await?;
        Ok(content)
    }

    /// Fetch the resolved action plan (already expanded into atomic steps).
    pub async fn fetch_action_metadata(&self, r#ref: &str) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/functions/v1/actions", self.base);
        let client = reqwest::Client::new();

        let mut req = client.get(&url).query(&[("ref", r#ref)]);
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }

        let resp = req.send().await?.error_for_status()?;
        Ok(resp.json::<serde_json::Value>().await?)
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
