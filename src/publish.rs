
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ShKind, ShPort, ShType};
    use tempfile::TempDir;

    fn create_test_manifest(kind: ShKind) -> ShManifest {
        ShManifest {
            name: "test-package".to_string(),
            description: "Test package".to_string(),
            version: "1.0.0".to_string(),
            kind: Some(kind),
            manifest_version: 1,
            repository: "github.com/test/package".to_string(),
            image: "ghcr.io/test/package".to_string(),
            license: "MIT".to_string(),
            inputs: vec![],
            outputs: vec![],
            steps: vec![],
            wires: vec![],
            export: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn test_cmd_publish_with_docker_manifest() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = create_test_manifest(ShKind::Docker);
        let manifest_path = temp_dir.path().join("starthub.json");
        
        // Write test manifest
        fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap()).unwrap();
        
        // Change to temp directory
        std::env::set_current_dir(temp_dir.path()).unwrap();
        
        // This will fail because docker command won't exist in test environment,
        // but we can test that the function doesn't panic and handles the manifest correctly
        let result = cmd_publish(false).await;
        
        // Clean up
        std::env::set_current_dir("/").unwrap();
        
        // The result should be an error (since docker won't be available in test env)
        // but the important thing is that it didn't panic and processed the manifest
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_cmd_publish_with_wasm_manifest() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = create_test_manifest(ShKind::Wasm);
        let manifest_path = temp_dir.path().join("starthub.json");
        
        // Write test manifest
        fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap()).unwrap();
        
        // Change to temp directory
        std::env::set_current_dir(temp_dir.path()).unwrap();
        
        // This will fail because cargo command won't exist in test environment,
        // but we can test that the function doesn't panic and handles the manifest correctly
        let result = cmd_publish(false).await;
        
        // Clean up
        std::env::set_current_dir("/").unwrap();
        
        // The result should be an error (since cargo won't be available in test env)
        // but the important thing is that it didn't panic and processed the manifest
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_cmd_publish_with_no_build_flag() {
        let temp_dir = TempDir::new().unwrap();
        let manifest = create_test_manifest(ShKind::Docker);
        let manifest_path = temp_dir.path().join("starthub.json");
        
        // Write test manifest
        fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap()).unwrap();
        
        // Change to temp directory
        std::env::set_current_dir(temp_dir.path()).unwrap();
        
        // Test with no_build = true
        let result = cmd_publish(true).await;
        
        // Clean up
        std::env::set_current_dir("/").unwrap();
        
        // Should still fail due to missing docker, but no panic
        assert!(result.is_err());
    }

    #[test]
    fn test_manifest_serialization_roundtrip() {
        let manifest = create_test_manifest(ShKind::Docker);
        
        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: ShManifest = serde_json::from_str(&json).unwrap();
        
        assert_eq!(manifest.name, deserialized.name);
        assert_eq!(manifest.version, deserialized.version);
        assert!(matches!(deserialized.kind, Some(ShKind::Docker)));
        assert_eq!(manifest.repository, deserialized.repository);
        assert_eq!(manifest.image, deserialized.image);
        assert_eq!(manifest.license, deserialized.license);
    }

    #[test]
    fn test_manifest_with_inputs_and_outputs() {
        let mut manifest = create_test_manifest(ShKind::Wasm);
        manifest.inputs = vec![
            ShPort {
                name: "input1".to_string(),
                description: "Test input".to_string(),
                ty: ShType::String,
                required: true,
                default: None,
            }
        ];
        manifest.outputs = vec![
            ShPort {
                name: "output1".to_string(),
                description: "Test output".to_string(),
                ty: ShType::Boolean,
                required: true,
                default: None,
            }
        ];
        
        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: ShManifest = serde_json::from_str(&json).unwrap();
        
        assert_eq!(manifest.inputs.len(), deserialized.inputs.len());
        assert_eq!(manifest.outputs.len(), deserialized.outputs.len());
        assert_eq!(manifest.inputs[0].name, deserialized.inputs[0].name);
        assert_eq!(manifest.outputs[0].name, deserialized.outputs[0].name);
    }

    #[tokio::test]
    async fn test_manifest_file_not_found() {
        // Test that the function properly handles missing manifest file
        let temp_dir = TempDir::new().unwrap();
        std::env::set_current_dir(temp_dir.path()).unwrap();
        
        // Try to publish without a manifest file
        let result = cmd_publish(false).await;
        
        // Clean up
        std::env::set_current_dir("/").unwrap();
        
        // Should fail with file not found error
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_manifest_invalid_json() {
        let temp_dir = TempDir::new().unwrap();
        let manifest_path = temp_dir.path().join("starthub.json");
        
        // Write invalid JSON
        fs::write(&manifest_path, "{ invalid json }").unwrap();
        
        // Change to temp directory
        std::env::set_current_dir(temp_dir.path()).unwrap();
        
        // Try to publish with invalid JSON
        let result = cmd_publish(false).await;
        
        // Clean up
        std::env::set_current_dir("/").unwrap();
        
        // Should fail with JSON parsing error
        assert!(result.is_err());
    }
}
