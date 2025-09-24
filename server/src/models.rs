use serde::{Serialize, Deserialize};

// ---- Starthub manifest schema ----
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShManifest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub version: String,
    pub kind: Option<ShKind>,
    pub manifest_version: u32,
    pub repository: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    pub license: String,
    pub inputs: Vec<ShPort>,
    pub outputs: Vec<ShPort>,
    // Custom type definitions
    #[serde(default)]
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub types: std::collections::HashMap<String, serde_json::Value>,
    // Composite action fields
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<ShActionStep>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub wires: Vec<ShWire>,
    #[serde(default)]
    #[serde(skip_serializing_if = "is_default_export")]
    pub export: serde_json::Value,
}

// Helper function to determine if export field should be skipped during serialization
fn is_default_export(export: &serde_json::Value) -> bool {
    export == &serde_json::json!({})
}

#[derive(Debug, Clone, PartialEq)]
pub enum ShKind { 
    Wasm, 
    Docker,
    Composition
}

// Custom serializer to output lowercase
impl serde::Serialize for ShKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let s = match self {
            ShKind::Wasm => "wasm",
            ShKind::Docker => "docker",
            ShKind::Composition => "composition",
        };
        serializer.serialize_str(s)
    }
}

impl<'de> serde::Deserialize<'de> for ShKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "wasm" => Ok(ShKind::Wasm),
            "docker" => Ok(ShKind::Docker),
            "composition" => Ok(ShKind::Composition),
            _ => Err(serde::de::Error::unknown_variant(&s, &["wasm", "docker", "composition"])),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShPort {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "type")]
    pub ty: ShType,
    #[serde(default = "default_required")]
    pub required: bool,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
}

fn default_required() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq)]
pub enum ShType {
    String,
    Number,
    Boolean,
    Object,
    Array,
}

impl serde::Serialize for ShType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let s = match self {
            ShType::String => "string",
            ShType::Number => "number",
            ShType::Boolean => "boolean",
            ShType::Object => "object",
            ShType::Array => "array",
        };
        serializer.serialize_str(s)
    }
}

impl<'de> serde::Deserialize<'de> for ShType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "string" => Ok(ShType::String),
            "number" => Ok(ShType::Number),
            "boolean" => Ok(ShType::Boolean),
            "object" => Ok(ShType::Object),
            "array" => Ok(ShType::Array),
            _ => Err(serde::de::Error::unknown_variant(&s, &["string", "number", "boolean", "object", "array"])),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShActionStep {
    pub id: String,
    pub uses: ShActionUses,
    #[serde(default)]
    pub with: std::collections::HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShActionUses {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShWire {
    pub from: ShWireFrom,
    pub to: ShWireTo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShWireFrom {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShWireTo {
    pub step: String,
    pub input: String,
}

// Execution-related types
#[derive(Debug, Clone)]
pub struct StepSpec {
    pub id: String,
    pub kind: String,
    pub ref_: String,
    pub args: Vec<String>,
    pub env: std::collections::HashMap<String, String>,
    pub workdir: Option<String>,
    pub network: Option<String>,
    pub entry: Option<String>,
    pub mounts: Vec<MountSpec>,
}

#[derive(Debug, Clone)]
pub struct MountSpec {
    pub typ: String,
    pub source: String,
    pub target: String,
    pub rw: bool,
}

#[derive(Debug, Clone)]
pub struct ActionPlan {
    pub steps: Vec<StepSpec>,
    pub workdir: Option<String>,
}

// API Client for StartHub
pub struct HubClient;

impl HubClient {
    pub fn new(_base_url: String, _token: Option<String>) -> Self {
        Self
    }


    pub async fn download_wasm(&self, action_ref: &str, cache_dir: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
        // Implementation for downloading WASM modules
        // This would download and cache the WASM module
        let wasm_path = cache_dir.join(format!("{}.wasm", action_ref.replace('/', "_")));
        if wasm_path.exists() {
            return Ok(wasm_path);
        }
        
        // TODO: Implement actual download logic
        Err(anyhow::anyhow!("WASM download not implemented yet"))
    }

    pub async fn download_starthub_lock(&self, storage_url: &str) -> anyhow::Result<ShManifest> {
        let client = reqwest::Client::new();
        let response = client.get(storage_url).send().await?;
        
        if response.status().is_success() {
            let manifest: ShManifest = response.json().await?;
            Ok(manifest)
        } else {
            Err(anyhow::anyhow!("Failed to download starthub-lock.json: {}", response.status()))
        }
    }
}
