
use std::fs;
use crate::models::ShManifest;
use crate::commands::{cmd_publish_docker_inner, cmd_publish_wasm_inner};

pub async fn cmd_publish(no_build: bool) -> anyhow::Result<()> {
    let manifest_str = fs::read_to_string("starthub.json")?;
    let m: ShManifest = serde_json::from_str(&manifest_str)?;

    match m.kind {
        Some(crate::models::ShKind::Docker) => cmd_publish_docker_inner(&m, no_build).await,
        Some(crate::models::ShKind::Wasm)   => cmd_publish_wasm_inner(&m, no_build).await,
        Some(crate::models::ShKind::Composition) => anyhow::bail!("Composition actions cannot be published directly"),
        None => anyhow::bail!("No kind specified in manifest"),
    }
}