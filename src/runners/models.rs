// src/runners/models.rs
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct ActionPlan {
    #[allow(dead_code)]
    pub id: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub version: String,
    #[serde(default)]
    pub workdir: Option<String>,
    pub steps: Vec<StepSpec>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct StepSpec {
    pub id: String,
    pub kind: String,       // "docker" | "wasm"
    #[serde(rename="ref")]
    pub ref_: String,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String,String>,
    #[serde(default)]
    pub mounts: Vec<MountSpec>,
    #[serde(default)]
    #[allow(dead_code)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub network: Option<String>, // docker only: "none"|"bridge"
    #[serde(default)]
    pub workdir: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MountSpec {
    #[serde(rename="type")]
    pub typ: String,  // "bind"
    pub source: String,
    pub target: String,
    pub rw: bool,
}
