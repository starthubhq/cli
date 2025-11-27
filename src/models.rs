use serde::{Serialize, Deserialize};

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
    #[serde(default)]
    pub interactive: bool,
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
    pub steps: std::collections::HashMap<String, ShActionStep>,
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

// Custom deserializer to handle both uppercase and lowercase values
impl<'de> serde::Deserialize<'de> for ShKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
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
    #[serde(default = "default_required", skip_serializing_if = "is_default_required")]
    pub required: bool,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
}

fn default_required() -> bool {
    true
}

fn is_default_required(value: &bool) -> bool {
    *value == default_required()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShLock {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub version: String,
    pub kind: ShKind,
    #[serde(default)]
    pub flow_control: bool,
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
    pub distribution: ShDistribution,
    pub digest: String,
    // Composition data for composite actions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub composition: Option<ShManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShDistribution {
    pub primary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ShType {
    String,
    Integer,
    Boolean,
    Object,
    Array,
    Number,
    Custom(String), // For custom types like "HttpHeaders", "HttpResponse", etc.
}

// Custom serializer for ShType
impl serde::Serialize for ShType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let s = match self {
            ShType::String => "string",
            ShType::Integer => "integer",
            ShType::Boolean => "boolean",
            ShType::Object => "object",
            ShType::Array => "array",
            ShType::Number => "number",
            ShType::Custom(custom) => custom,
        };
        serializer.serialize_str(s)
    }
}

// Custom deserializer for ShType
impl<'de> serde::Deserialize<'de> for ShType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(ShType::from_str(&s))
    }
}

impl ShType {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "string" => ShType::String,
            "integer" => ShType::Integer,
            "boolean" => ShType::Boolean,
            "object" => ShType::Object,
            "array" => ShType::Array,
            "number" => ShType::Number,
            _ => ShType::Custom(s.to_string()),
        }
    }
    
    pub fn to_string(&self) -> String {
        match self {
            ShType::String => "string".to_string(),
            ShType::Integer => "integer".to_string(),
            ShType::Boolean => "boolean".to_string(),
            ShType::Object => "object".to_string(),
            ShType::Array => "array".to_string(),
            ShType::Number => "number".to_string(),
            ShType::Custom(name) => name.clone(),
        }
    }
}


// Composite action structures
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShActionStep {
    pub id: String,
    #[serde(default)]
    pub kind: Option<String>, // "docker" (default) or "wasm"
    pub uses: ShActionUses,
    #[serde(default)]
    pub with: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShActionUses {
    pub name: String,
    #[serde(default)]
    pub types: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShWireFrom {
    #[serde(default)]
    pub source: Option<String>, // "inputs"
    #[serde(default)]
    pub step: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub value: Option<serde_json::Value>, // literal
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShWireTo {
    pub step: String,
    pub input: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShWire {
    pub from: ShWireFrom,
    pub to: ShWireTo,
}