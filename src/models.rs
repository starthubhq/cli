use serde::{Serialize, Deserialize};

// ---- Starthub manifest schema ----
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShManifest {
    pub name: String,
    pub version: String,
    pub kind: ShKind,
    pub manifest_version: u32,
    pub repository: String,
    pub image: String,
    pub license: String,
    pub inputs: Vec<ShPort>,
    pub outputs: Vec<ShPort>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ShKind { 
    Wasm, 
    Docker 
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShPort {
    pub name: String,
    pub description: String,
    #[serde(rename = "type")]
    pub ty: ShType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShLock {
    pub name: String,
    pub version: String,
    pub kind: ShKind,
    pub distribution: ShDistribution,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShDistribution {
    pub primary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ShType {
    String,
    Integer,
    Boolean,
    Object,
    Array,
    Number,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn test_sh_manifest_serialization() {
        let manifest = ShManifest {
            name: "test-package".to_string(),
            version: "1.0.0".to_string(),
            kind: ShKind::Wasm,
            repository: "github.com/test/package".to_string(),
            image: "ghcr.io/test/package".to_string(),
            license: "MIT".to_string(),
            inputs: vec![],
            outputs: vec![],
        };

        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: ShManifest = serde_json::from_str(&json).unwrap();

        assert_eq!(manifest.name, deserialized.name);
        assert_eq!(manifest.version, deserialized.version);
        assert!(matches!(deserialized.kind, ShKind::Wasm));
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
        };

        let json = serde_json::to_string(&port).unwrap();
        let deserialized: ShPort = serde_json::from_str(&json).unwrap();

        assert_eq!(port.name, deserialized.name);
        assert_eq!(port.description, deserialized.description);
        assert!(matches!(deserialized.ty, ShType::String));
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
            version: "1.0.0".to_string(),
            kind: ShKind::Docker,
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
            version: "1.0.0".to_string(),
            kind: ShKind::Docker,
            manifest_version: 1,
            repository: "github.com/test/package".to_string(),
            image: "ghcr.io/test/package".to_string(),
            license: "MIT".to_string(),
            inputs: vec![
                ShPort {
                    name: "input1".to_string(),
                    description: "First input".to_string(),
                    ty: ShType::String,
                },
                ShPort {
                    name: "input2".to_string(),
                    description: "Second input".to_string(),
                    ty: ShType::Integer,
                },
            ],
            outputs: vec![
                ShPort {
                    name: "output1".to_string(),
                    description: "First output".to_string(),
                    ty: ShType::Boolean,
                },
            ],
        };

        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: ShManifest = serde_json::from_str(&json).unwrap();

        assert_eq!(manifest.inputs.len(), deserialized.inputs.len());
        assert_eq!(manifest.outputs.len(), deserialized.outputs.len());
        assert_eq!(manifest.inputs[0].name, deserialized.inputs[0].name);
        assert_eq!(manifest.outputs[0].name, deserialized.outputs[0].name);
    }
}
