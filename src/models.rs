use serde::{Serialize, Deserialize};

// ---- Starthub manifest schema ----
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShManifest {
    pub name: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ShKind { 
    Wasm, 
    Docker,
    Composition
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShPort {
    pub name: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShLock {
    pub name: String,
    pub description: String,
    pub version: String,
    pub kind: ShKind,
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

impl serde::Serialize for ShType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for ShType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(ShType::from_str(&s))
    }
}

// Composite action structures
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShActionStep {
    pub id: String,
    #[serde(default)]
    pub kind: Option<String>, // "docker" (default) or "wasm"
    pub uses: String,
    #[serde(default)]
    pub with: std::collections::HashMap<String, serde_json::Value>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn test_sh_manifest_serialization() {
        let manifest = ShManifest {
            name: "test-package".to_string(),
            description: "Test package".to_string(),
            version: "1.0.0".to_string(),
            kind: Some(ShKind::Wasm),
            manifest_version: 1,
            repository: "github.com/test/package".to_string(),
            image: Some("ghcr.io/test/package".to_string()),
            license: "MIT".to_string(),
            inputs: vec![],
            outputs: vec![],
            types: std::collections::HashMap::new(),
            steps: vec![],
            wires: vec![],
            export: serde_json::json!({}),
        };

        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: ShManifest = serde_json::from_str(&json).unwrap();

        assert_eq!(manifest.name, deserialized.name);
        assert_eq!(manifest.version, deserialized.version);
        assert!(matches!(deserialized.kind, Some(ShKind::Wasm)));
        assert_eq!(manifest.repository, deserialized.repository);
        assert_eq!(manifest.image, deserialized.image);
        assert_eq!(manifest.license, deserialized.license);
    }

    #[test]
    fn test_sh_kind_serialization() {
        let wasm_kind = ShKind::Wasm;
        let docker_kind = ShKind::Docker;

        let wasm_json = serde_json::to_string(&wasm_kind).unwrap();
        let docker_json = serde_json::to_string(&docker_kind).unwrap();

        assert_eq!(wasm_json, r#""wasm""#);
        assert_eq!(docker_json, r#""docker""#);

        let deserialized_wasm: ShKind = serde_json::from_str(&wasm_json).unwrap();
        let deserialized_docker: ShKind = serde_json::from_str(&docker_json).unwrap();

        assert!(matches!(deserialized_wasm, ShKind::Wasm));
        assert!(matches!(deserialized_docker, ShKind::Docker));
    }

    #[test]
    fn test_sh_port_serialization() {
        let port = ShPort {
            name: "input_param".to_string(),
            description: "Test input parameter".to_string(),
            ty: ShType::String,
            required: true,
            default: None,
        };

        let json = serde_json::to_string(&port).unwrap();
        let deserialized: ShPort = serde_json::from_str(&json).unwrap();

        assert_eq!(port.name, deserialized.name);
        assert_eq!(port.description, deserialized.description);
        assert!(matches!(deserialized.ty, ShType::String));
        assert_eq!(port.required, deserialized.required);
        assert_eq!(port.default, deserialized.default);
    }

    #[test]
    fn test_sh_port_with_default_values() {
        let port = ShPort {
            name: "optional_param".to_string(),
            description: "Optional parameter with default".to_string(),
            ty: ShType::String,
            required: false,
            default: Some(serde_json::json!("default_value")),
        };

        let json = serde_json::to_string(&port).unwrap();
        let deserialized: ShPort = serde_json::from_str(&json).unwrap();

        assert_eq!(port.required, deserialized.required);
        assert_eq!(port.default, deserialized.default);
    }

    #[test]
    fn test_sh_type_serialization() {
        let types = vec![
            ShType::String,
            ShType::Integer,
            ShType::Boolean,
            ShType::Object,
            ShType::Array,
            ShType::Number,
        ];

        for sh_type in types {
            let json = serde_json::to_string(&sh_type).unwrap();
            let deserialized: ShType = serde_json::from_str(&json).unwrap();
            assert!(matches!(deserialized, _));
        }
    }

    #[test]
    fn test_sh_lock_serialization() {
        let lock = ShLock {
            name: "test-package".to_string(),
            description: "Test package".to_string(),
            version: "1.0.0".to_string(),
            kind: ShKind::Docker,
            manifest_version: 1,
            repository: "github.com/test/package".to_string(),
            image: Some("ghcr.io/test/package".to_string()),
            license: "MIT".to_string(),
            inputs: vec![],
            outputs: vec![],
            types: std::collections::HashMap::new(),
            distribution: ShDistribution {
                primary: "oci://ghcr.io/test/package@sha256:abc123".to_string(),
                upstream: None,
            },
            digest: "sha256:abc123".to_string(),
        };

        let json = serde_json::to_string(&lock).unwrap();
        let deserialized: ShLock = serde_json::from_str(&json).unwrap();

        assert_eq!(lock.name, deserialized.name);
        assert_eq!(lock.version, deserialized.version);
        assert!(matches!(deserialized.kind, ShKind::Docker));
        assert_eq!(lock.distribution.primary, deserialized.distribution.primary);
        assert_eq!(lock.digest, deserialized.digest);
    }

    #[test]
    fn test_sh_distribution_with_upstream() {
        let distribution = ShDistribution {
            primary: "oci://ghcr.io/test/package@sha256:abc123".to_string(),
            upstream: Some("oci://docker.io/test/package@sha256:abc123".to_string()),
        };

        let json = serde_json::to_string(&distribution).unwrap();
        let deserialized: ShDistribution = serde_json::from_str(&json).unwrap();

        assert_eq!(distribution.primary, deserialized.primary);
        assert_eq!(distribution.upstream, deserialized.upstream);
    }

    #[test]
    fn test_sh_distribution_without_upstream() {
        let distribution = ShDistribution {
            primary: "oci://ghcr.io/test/package@sha256:abc123".to_string(),
            upstream: None,
        };

        let json = serde_json::to_string(&distribution).unwrap();
        let deserialized: ShDistribution = serde_json::from_str(&json).unwrap();

        assert_eq!(distribution.primary, deserialized.primary);
        assert_eq!(distribution.upstream, deserialized.upstream);
    }

    #[test]
    fn test_manifest_with_inputs_and_outputs() {
        let manifest = ShManifest {
            name: "test-package".to_string(),
            description: "Test package with inputs and outputs".to_string(),
            version: "1.0.0".to_string(),
            kind: Some(ShKind::Docker),
            manifest_version: 1,
            repository: "github.com/test/package".to_string(),
            image: Some("ghcr.io/test/package".to_string()),
            license: "MIT".to_string(),
            inputs: vec![
                ShPort {
                    name: "input1".to_string(),
                    description: "First input".to_string(),
                    ty: ShType::String,
                    required: true,
                    default: None,
                },
                ShPort {
                    name: "input2".to_string(),
                    description: "Second input".to_string(),
                    ty: ShType::Integer,
                    required: false,
                    default: Some(serde_json::json!(42)),
                },
            ],
            outputs: vec![
                ShPort {
                    name: "output1".to_string(),
                    description: "First output".to_string(),
                    ty: ShType::Boolean,
                    required: true,
                    default: None,
                },
            ],
            types: std::collections::HashMap::new(),
            steps: vec![],
            wires: vec![],
            export: serde_json::json!({}),
        };

        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: ShManifest = serde_json::from_str(&json).unwrap();

        assert_eq!(manifest.inputs.len(), deserialized.inputs.len());
        assert_eq!(manifest.outputs.len(), deserialized.outputs.len());
        assert_eq!(manifest.inputs[0].name, deserialized.inputs[0].name);
        assert_eq!(manifest.outputs[0].name, deserialized.outputs[0].name);
    }

    #[test]
    fn test_custom_types() {
        let mut types = std::collections::HashMap::new();
        types.insert("HttpHeaders".to_string(), serde_json::json!({
            "Content-Type": "string",
            "Authorization": "string",
            "User-Agent": "string"
        }));
        types.insert("HttpResponse".to_string(), serde_json::json!({
            "status": "number",
            "body": "string"
        }));

        let manifest = ShManifest {
            name: "test-package".to_string(),
            description: "Test package with custom types".to_string(),
            version: "1.0.0".to_string(),
            kind: Some(ShKind::Wasm),
            manifest_version: 1,
            repository: "github.com/test/package".to_string(),
            image: Some("ghcr.io/test/package".to_string()),
            license: "MIT".to_string(),
            inputs: vec![
                ShPort {
                    name: "headers".to_string(),
                    description: "HTTP headers".to_string(),
                    ty: ShType::Custom("HttpHeaders".to_string()),
                    required: false,
                    default: None,
                },
            ],
            outputs: vec![
                ShPort {
                    name: "response".to_string(),
                    description: "HTTP response".to_string(),
                    ty: ShType::Custom("HttpResponse".to_string()),
                    required: true,
                    default: None,
                },
            ],
            types,
            steps: vec![],
            wires: vec![],
            export: serde_json::json!({}),
        };

        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: ShManifest = serde_json::from_str(&json).unwrap();

        assert_eq!(manifest.types.len(), deserialized.types.len());
        assert!(deserialized.types.contains_key("HttpHeaders"));
        assert!(deserialized.types.contains_key("HttpResponse"));
        assert!(matches!(deserialized.inputs[0].ty, ShType::Custom(ref name) if name == "HttpHeaders"));
        assert!(matches!(deserialized.outputs[0].ty, ShType::Custom(ref name) if name == "HttpResponse"));
    }

    #[test]
    fn test_sh_type_custom_serialization() {
        let custom_type = ShType::Custom("HttpHeaders".to_string());
        let json = serde_json::to_string(&custom_type).unwrap();
        assert_eq!(json, r#""HttpHeaders""#);

        let deserialized: ShType = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, ShType::Custom(ref name) if name == "HttpHeaders"));
    }
}
