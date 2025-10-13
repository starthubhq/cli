use std::collections::HashMap;

use serde::{Serialize, Deserialize};
use serde_json::Value;

// ---- Starthub manifest schema ----
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShManifest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub version: String,
    pub kind: Option<ShKind>,
    #[serde(default)]
    pub flow_control: bool,
    pub manifest_version: u32,
    pub repository: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    pub license: String,
    pub inputs: serde_json::Value,
    pub outputs: serde_json::Value,
    // Custom type definitions
    #[serde(default)]
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub types: std::collections::HashMap<String, serde_json::Value>,
    // Composite action fields - steps are now an object with step_id as key
    #[serde(default)]
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub steps: std::collections::HashMap<String, serde_json::Value>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub wires: Vec<ShWire>,
    #[serde(default)]
    #[serde(skip_serializing_if = "is_default_export")]
    pub export: serde_json::Value,
    // Mirrors for artifact downloads
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mirrors: Vec<String>,
    // Permissions for the action
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<ShPermissions>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShPermissions {
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fs: Vec<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub net: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShIO {
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub template: Value,
    pub required: bool,
}

// Data flow edge representing a variable dependency between steps
#[derive(Debug, Clone, serde::Serialize)]
pub struct ShAction {
    pub id: String,
    pub name: String,                    // "get_coordinates" or "get_weather_response"
    pub kind: String,                    // "composition", "wasm", "docker"
    pub uses: String,                    // Reference to the action
    pub inputs: Vec<ShIO>,              // Array format: [{"name": "...", "type": "...", "value": ...}]
    pub outputs: Vec<ShIO>,             // Array format: [{"name": "...", "type": "...", "value": ...}]
    pub parent_action: Option<String>,   // UUID of parent action (None for root)
    pub steps: HashMap<String, ShAction>, // Nested actions keyed by UUID
    pub flow_control: bool,               // Flow control capability
    
    // Manifest structure fields
    pub types: Option<serde_json::Map<String, Value>>,   // From manifest.types
    pub mirrors: Vec<String>,           // Mirrors for artifact downloads
    pub permissions: Option<ShPermissions>, // Permissions for the action
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

#[derive(Debug, Clone, PartialEq)]
pub enum ShType {
    String,
    Number,
    Boolean,
    Object,
    Array,
    // Custom types are allowed
    Custom(String),
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
            ShType::Custom(custom_type) => custom_type,
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
            custom_type => Ok(ShType::Custom(custom_type.to_string())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShWire {
    pub from: ShWireFrom,
    pub to: ShWireTo,
}

// Execution context structures
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShExecutionFrame {
    pub id: String,
    pub name: String,
    pub uses: String,
    pub inputs: Vec<serde_json::Value>,
    pub outputs: Vec<serde_json::Value>,
    pub frame: Option<Box<ShExecutionFrame>>,
    #[serde(skip_serializing, skip_deserializing)]
    pub parent: Option<std::sync::Weak<ShExecutionFrame>>,
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
    pub step_definition: Option<serde_json::Value>,
    pub calling_step_definition: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct MountSpec {
    pub typ: String,
    pub source: String,
    pub target: String,
    pub rw: bool,
}


// API Client for StartHub
pub struct HubClient;

impl HubClient {
    pub fn new(_base_url: String, _token: Option<String>) -> Self {
        Self
    }


    pub async fn download_wasm(&self, action_ref: &str, cache_dir: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
        
        // Ensure base cache directory exists
        if let Err(e) = std::fs::create_dir_all(cache_dir) {
            return Err(anyhow::anyhow!("Failed to create base cache directory {:?}: {}", cache_dir, e));
        }
        
        // Convert action_ref from "org/name:version" to "org/name/version" format
        // Also strip "github.com/" prefix if present
        let url_path = action_ref
            .replace("github.com/", "")
            .replace(":", "/");
        let artifacts_url = format!(
            "https://api.starthub.so/storage/v1/object/public/artifacts/{}/artifact.zip",
            url_path
        );
        
        
        // Create action-specific cache directory
        let action_cache_dir = cache_dir.join(action_ref.replace('/', "_").replace(":", "_"));
        
        // Create directory if it doesn't exist
        if let Err(e) = std::fs::create_dir_all(&action_cache_dir) {
            return Err(anyhow::anyhow!("Failed to create cache directory {:?}: {}", action_cache_dir, e));
        }
        
        // Download the artifacts zip file
        let response = reqwest::get(&artifacts_url).await?;
        if !response.status().is_success() {
            return Err(anyhow::anyhow!("Failed to download artifacts from {}", artifacts_url));
        }
        
        let zip_data = response.bytes().await?;
        
        // Extract the zip file
        let cursor = std::io::Cursor::new(zip_data);
        let mut archive = zip::ZipArchive::new(cursor)?;
        archive.extract(&action_cache_dir)?;
        
        // Find the WASM file (could be main.wasm, or named after the action)
        let wasm_files: Vec<_> = std::fs::read_dir(&action_cache_dir)?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                if let Some(path) = entry.path().file_name() {
                    if let Some(name) = path.to_str() {
                        return name.ends_with(".wasm");
                    }
                }
                false
            })
            .collect();
        
        
        if wasm_files.is_empty() {
            return Err(anyhow::anyhow!("No WASM file found in extracted artifacts"));
        }
        
        // Rename the WASM file to "artifact.wasm" for consistency
        let original_wasm_path = wasm_files[0].path();
        let artifact_path = action_cache_dir.join("artifact.wasm");
        
        // Only rename if the paths are different
        if original_wasm_path != artifact_path {
            // Remove existing artifact.wasm if it exists
            if artifact_path.exists() {
                std::fs::remove_file(&artifact_path)?;
            }
            
            // Rename the found WASM file to artifact.wasm
            std::fs::rename(&original_wasm_path, &artifact_path)?;
        }
        
        // Verify the final file exists and is accessible
        if !artifact_path.exists() {
            return Err(anyhow::anyhow!("Failed to create artifact.wasm at {:?}", artifact_path));
        }
        
        // Check file permissions
        if let Err(e) = std::fs::metadata(&artifact_path) {
            return Err(anyhow::anyhow!("Artifact.wasm not accessible at {:?}: {}", artifact_path, e));
        }
        
        Ok(artifact_path)
    }

}