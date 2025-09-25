use anyhow::Result;
use std::{fs, path::Path};
use std::process::Command as PCommand;
use inquire::{Text, Select, Confirm};
use tokio::time::{sleep, Duration};
use webbrowser;
use aws_config::meta::region::RegionProviderChain;
use aws_sdk_s3::{config::Region, Client as S3Client};
use aws_sdk_s3::primitives::ByteStream;
use reqwest;

use crate::models::{ShManifest, ShKind, ShPort, ShLock, ShDistribution, ShType};
use crate::templates;

// Global constants for local development server
// Change these if you need to use a different port or host
const LOCAL_SERVER_URL: &str = "http://127.0.0.1:3000";
const LOCAL_SERVER_HOST: &str = "127.0.0.1:3000";

use serde_json::{Value, json};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;


impl AppState {
    fn new() -> Self {
        let (ws_sender, _) = broadcast::channel(100);
        Self { 
            ws_sender,
            types_storage: std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
            execution_orders: std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
            composition_data: std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        }
    }
    
    /// Store types from a lock file in the global types storage
    fn store_types(&self, action_ref: &str, types: &std::collections::HashMap<String, serde_json::Value>) {
        if let Ok(mut storage) = self.types_storage.write() {
            for (type_name, type_schema) in types {
                let key = format!("{}:{}", action_ref, type_name);
                storage.insert(key, type_schema.clone());
                println!("📋 Stored type: {} from action: {}", type_name, action_ref);
            }
        }
    }
    
    /// Get all stored types
    fn get_all_types(&self) -> std::collections::HashMap<String, serde_json::Value> {
        match self.types_storage.read() {
            Ok(storage) => storage.clone(),
            Err(_) => std::collections::HashMap::new(),
        }
    }
    
    /// Get types for a specific action
    fn get_types_for_action(&self, action_ref: &str) -> std::collections::HashMap<String, serde_json::Value> {
        match self.types_storage.read() {
            Ok(storage) => {
                let prefix = format!("{}:", action_ref);
                storage.iter()
                    .filter(|(key, _)| key.starts_with(&prefix))
                    .map(|(key, value)| {
                        let type_name = key.strip_prefix(&prefix).unwrap_or(key);
                        (type_name.to_string(), value.clone())
                    })
                    .collect()
            }
            Err(_) => std::collections::HashMap::new(),
        }
    }
    
    /// Store execution order for a composite action
    fn store_execution_order(&self, action_ref: &str, execution_order: Vec<String>) {
        if let Ok(mut orders) = self.execution_orders.write() {
            orders.insert(action_ref.to_string(), execution_order.clone());
            println!("📋 Stored execution order for {}: {:?}", action_ref, execution_order);
        }
    }
    
    /// Get execution order for a specific action
    fn get_execution_order(&self, action_ref: &str) -> Option<Vec<String>> {
        match self.execution_orders.read() {
            Ok(orders) => orders.get(action_ref).cloned(),
            Err(_) => None,
        }
    }
    
    /// Get all execution orders
    fn get_all_execution_orders(&self) -> std::collections::HashMap<String, Vec<String>> {
        match self.execution_orders.read() {
            Ok(orders) => orders.clone(),
            Err(_) => std::collections::HashMap::new(),
        }
    }
    
    /// Store composition data for a composite action
    fn store_composition_data(&self, action_ref: &str, composition: crate::models::ShManifest) {
        if let Ok(mut compositions) = self.composition_data.write() {
            compositions.insert(action_ref.to_string(), composition);
            println!("📋 Stored composition data for: {}", action_ref);
        }
    }
    
    /// Get composition data for a specific action
    fn get_composition_data(&self, action_ref: &str) -> Option<crate::models::ShManifest> {
        match self.composition_data.read() {
            Ok(compositions) => compositions.get(action_ref).cloned(),
            Err(_) => None,
        }
    }
}







// ============================================================================
// HELPER FUNCTIONS FOR ACTION RESOLUTION
// ============================================================================


/// Recursively fetch all action lock files for a composite action
fn fetch_all_action_locks<'a>(
    _client: &'a crate::starthub_api::Client,
    action_ref: &'a str,
    visited: &'a mut std::collections::HashSet<String>
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<crate::models::ShLock>>> + Send + 'a>> {
    Box::pin(async move {
    if visited.contains(action_ref) {
        return Ok(vec![]); // Already fetched this action
    }
    visited.insert(action_ref.to_string());
    
    // Construct the lock file URL based on the action reference
    // Format: https://api.starthub.so/storage/v1/object/public/artifacts/{owner}/{name}/{version}/lock.json
    let parts: Vec<&str> = action_ref.split('/').collect();
    if parts.len() < 2 {
        return Err(anyhow::anyhow!("Invalid action reference format: {}", action_ref));
    }
    
    let owner = parts[0];
    let name_version = parts[1];
    let (name, version) = if name_version.contains('@') {
        let name_version_parts: Vec<&str> = name_version.split('@').collect();
        (name_version_parts[0], name_version_parts[1])
    } else {
        return Err(anyhow::anyhow!("Action reference must include version: {}", action_ref));
    };
    
    let lock_url = format!(
        "https://api.starthub.so/storage/v1/object/public/artifacts/{}/{}/{}/lock.json",
        owner, name, version
    );
    
    println!("🔗 Fetching lock file: {}", lock_url);
    
    // Download and parse the lock file
    let response = reqwest::get(&lock_url).await?;
    if !response.status().is_success() {
        return Err(anyhow::anyhow!("Failed to fetch lock file: HTTP {}", response.status()));
    }
    
    let lock_content = response.text().await?;
    println!("🔍 Lock file content preview: {}", &lock_content[..std::cmp::min(500, lock_content.len())]);
    
    // Try to parse as JSON first to see the structure
    let json_value: serde_json::Value = serde_json::from_str(&lock_content)?;
    println!("🔍 Parsed JSON structure keys: {:?}", json_value.as_object().map(|obj| obj.keys().collect::<Vec<_>>()));
    
    let lock: crate::models::ShLock = serde_json::from_str(&lock_content)?;
    
    let mut all_locks = vec![lock.clone()];
    
    // If this is a composite action, recursively fetch locks for all steps
    if lock.kind == crate::models::ShKind::Composition {
        println!("🔄 Composite action detected, fetching manifest for step resolution...");
        
        // Fetch the manifest to get the steps
        let manifest_url = format!(
            "https://api.starthub.so/storage/v1/object/public/git/{}/{}/starthub.json",
            owner, name
        );
        
        println!("🔗 Fetching manifest: {}", manifest_url);
        
        match reqwest::get(&manifest_url).await {
            Ok(response) => {
                if response.status().is_success() {
                    match response.text().await {
                        Ok(manifest_content) => {
                            match serde_json::from_str::<crate::models::ShManifest>(&manifest_content) {
                                Ok(manifest) => {
                                    println!("✅ Successfully parsed manifest with {} steps", manifest.steps.len());
                                    
                                    // Recursively fetch locks for each step
    for step in &manifest.steps {
                                        let step_ref = &step.uses.name;
                                        println!("🔍 Processing step: {}", step_ref);
                                        
                                        match fetch_all_action_locks(_client, step_ref, visited).await {
                                            Ok(step_locks) => {
                                                let step_count = step_locks.len();
                                                all_locks.extend(step_locks);
                                                println!("✅ Added {} lock(s) from step: {}", step_count, step_ref);
                                            }
                                            Err(e) => {
                                                println!("⚠️  Failed to fetch locks for step {}: {}", step_ref, e);
                                                // Continue with other steps even if one fails
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    println!("⚠️  Failed to parse manifest JSON: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            println!("⚠️  Failed to read manifest content: {}", e);
                        }
                    }
                } else {
                    println!("⚠️  Failed to fetch manifest: HTTP {}", response.status());
                }
            }
            Err(e) => {
                println!("⚠️  Failed to fetch manifest: {}", e);
            }
        }
    }
    
    Ok(all_locks)
    })
}

/// Compute execution order for composite actions using topological sort
async fn compute_execution_order(
    action_ref: &str,
    lock: &crate::models::ShLock
) -> Result<Option<Vec<String>>> {
    // Only composite actions have execution order
    if lock.kind != crate::models::ShKind::Composition {
        return Ok(None);
    }
    
    println!("🔄 Computing execution order for composite action: {}", action_ref);
    
    // Check if the lock file has composition data embedded
    if lock.composition.is_none() {
        return Err(anyhow::anyhow!("No composition data found in lock file"));
    }
    
    let composition = lock.composition.as_ref().unwrap();
    
    if composition.steps.is_empty() {
        println!("⚠️  No steps found in composition");
        return Ok(Some(vec![]));
    }
    
    println!("📋 Found {} steps in composition", composition.steps.len());
    
    // Use the steps and wires directly from the composition
    let steps: Vec<crate::runners::local::ActionStep> = composition.steps.clone();
    let wires: Vec<crate::runners::local::Wire> = composition.wires.clone();
    
    // Use the existing topological sort function
    match crate::runners::local::topo_order(&steps, &wires) {
        Ok(order) => {
            println!("✅ Computed execution order: {:?}", order);
            Ok(Some(order))
        }
        Err(e) => {
            println!("❌ Failed to compute execution order: {}", e);
            Err(e)
        }
    }
}

/// Execute the ordered steps for a composite action
async fn execute_ordered_steps(
    action_ref: &str,
    execution_order: &[String],
    inputs: &std::collections::HashMap<String, serde_json::Value>,
    artifacts_dir: &std::path::Path,
    state: &AppState
) -> Result<serde_json::Value> {
    println!("🚀 Starting execution of {} steps for action: {}", execution_order.len(), action_ref);
    
    // Get the composition data from the state (we already have it from the lock file)
    let composition = state.get_composition_data(action_ref)
        .ok_or_else(|| anyhow::anyhow!("No composition data found for action: {}", action_ref))?;
    
    // Store step outputs as we execute them
    let mut step_outputs: std::collections::HashMap<String, serde_json::Value> = std::collections::HashMap::new();
    
    // Execute each step in order
    for step_id in execution_order {
        println!("🔄 Executing step: {}", step_id);
        
        // Find the step definition
        let step = composition.steps.iter()
            .find(|s| s.id == *step_id)
            .ok_or_else(|| anyhow::anyhow!("Step {} not found in composition", step_id))?;
        
        // Build inputs for this step based on wires
        let step_inputs = build_step_inputs(step_id, &composition.wires, inputs, &step_outputs)?;
        println!("🔍 Built inputs for step {}: {:?}", step_id, step_inputs);
        
        // Execute the step
        let step_output = execute_single_step(step, &step_inputs, artifacts_dir).await?;
        
        // Store the output for future steps
        step_outputs.insert(step_id.clone(), step_output.clone());
        println!("🔍 Stored output for step {}: {:?}", step_id, step_output);
        
        println!("✅ Step {} completed successfully", step_id);
    }
    
    // Build final outputs based on export configuration
    let final_outputs = build_final_outputs(&composition.export, &step_outputs)?;
    
    println!("🎉 All steps executed successfully!");
    Ok(final_outputs)
}

/// Build inputs for a specific step based on wires
fn build_step_inputs(
    step_id: &str,
    wires: &[crate::models::ShWire],
    action_inputs: &std::collections::HashMap<String, serde_json::Value>,
    step_outputs: &std::collections::HashMap<String, serde_json::Value>
) -> Result<Vec<serde_json::Value>> {
    let mut input_values: std::collections::HashMap<String, serde_json::Value> = std::collections::HashMap::new();
    
    // Find all wires that target this step
    for wire in wires {
        if wire.to.step == step_id {
            let value = match &wire.from {
                // Input from action inputs
                crate::models::ShWireFrom { source: Some(source), key: Some(key), .. } 
                    if source == "inputs" => {
                        action_inputs.get(key).cloned()
                    }
                // Input from previous step output - extract from positional array
                crate::models::ShWireFrom { step: Some(from_step), output: Some(output), .. } => {
                    // Get the step output (which is a positional array)
                    let mut found_value = None;
                    if let Some(step_output) = step_outputs.get(from_step) {
                        // The step_output should be an array like [{"status":200},{"body":"..."}]
                        if let Some(output_array) = step_output.as_array() {
                            // Find the output by name in the array
                            for item in output_array {
                                if let Some(obj) = item.as_object() {
                                    if obj.contains_key(output) {
                                        found_value = obj.get(output).cloned();
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    found_value
                }
                // Literal value
                crate::models::ShWireFrom { value: Some(value), .. } => {
                    Some(value.clone())
                }
                _ => None,
            };
            
            if let Some(value) = value {
                input_values.insert(wire.to.input.clone(), value);
            }
        }
    }
    
    // Convert to array of objects format that WASM expects
    // Each input becomes an object with the input name as key
    // For now, we'll hardcode the correct order for known steps
    let mut inputs_array = Vec::new();
    
    // Define the correct input order for each step
    let input_order = match step_id {
        "http_get_wasm" => vec!["url", "headers"],
        "stringify_wasm" => vec!["object"],
        "parse_wasm" => vec!["string", "type"],
        _ => {
            // Fallback: use the order from input_values
            input_values.keys().map(|s| s.as_str()).collect()
        }
    };
    
    // Build inputs in the correct order
    for input_name in input_order {
        if let Some(value) = input_values.get(input_name) {
            inputs_array.push(json!({input_name: value}));
        }
    }
    
    Ok(inputs_array)
}

/// Execute a simple WASM action
async fn execute_simple_wasm_action(
    action_ref: &str,
    inputs: &Vec<serde_json::Value>,
    artifacts_dir: &std::path::Path
) -> Result<serde_json::Value> {
    println!("🚀 Executing simple WASM action: {}", action_ref);
    
    // Create a safe filename from the action reference
    let safe_name = action_ref.replace('/', "_").replace('@', "_");
    let artifact_path = artifacts_dir.join(format!("{}.wasm", safe_name));
    
    if !artifact_path.exists() {
        return Err(anyhow::anyhow!("WASM artifact not found: {}", artifact_path.display()));
    }
    
    // Execute the WASM file
    execute_wasm_file(&artifact_path, inputs).await
}

/// Execute a single step (WASM or Docker)
async fn execute_single_step(
    step: &crate::models::ShActionStep,
    inputs: &Vec<serde_json::Value>,
    artifacts_dir: &std::path::Path
) -> Result<serde_json::Value> {
    println!("🔧 Executing step: {} (uses: {})", step.id, step.uses.name);
    
    // For now, we'll focus on WASM steps
    // TODO: Add Docker step execution later
    
    // Find the corresponding WASM artifact
    let safe_name = step.uses.name.replace('/', "_").replace(':', "_");
    let artifact_path = artifacts_dir.join(format!("{}.wasm", safe_name));
    
    if !artifact_path.exists() {
        return Err(anyhow::anyhow!("WASM artifact not found: {}", artifact_path.display()));
    }
    
    // Execute the WASM file (inputs are already in the correct format)
    execute_wasm_file(&artifact_path, inputs).await
}

/// Execute a WASM file using wasmtime command line
async fn execute_wasm_file(
    wasm_path: &std::path::Path,
    inputs: &Vec<serde_json::Value>
) -> Result<serde_json::Value> {
    println!("🔧 Executing WASM file: {}", wasm_path.display());
    
    // Inputs are already in array format, use them directly
    let inputs_json = serde_json::to_string(inputs)?;
    println!("📥 Inputs: {}", inputs_json);
    
    // Use wasmtime command line - only enable HTTP support for modules that need it
    let mut cmd = tokio::process::Command::new("wasmtime");
    
    // Check if this is an HTTP-related module that needs HTTP support
    let needs_http = wasm_path.to_string_lossy().contains("http-get-wasm");
    
    if needs_http {
        cmd.arg("-S").arg("http");  // Enable HTTP support for WASI HTTP
    }
    
    let mut output = cmd.arg(wasm_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    
    // Write inputs to stdin
    if let Some(stdin) = output.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let mut stdin = stdin;
        stdin.write_all(inputs_json.as_bytes()).await?;
        stdin.flush().await?;
    }
    
    // Wait for completion and capture output
    let result = output.wait_with_output().await?;
    
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    
    println!("📤 WASM stdout: {}", stdout);
    if !stderr.is_empty() {
        println!("📤 WASM stderr: {}", stderr);
    }
    
    if result.status.success() {
        println!("✅ WASM execution completed successfully");
        
        // Parse the output to extract the result
        // The WASM outputs in format: ::starthub:state::{json}
        let mut final_output = json!([
            {"status": "success"},
            {"message": "WASM executed successfully"},
            {"inputs": inputs}
        ]);
        
        // Try to extract the starthub state output
        for line in stdout.lines() {
            if line.starts_with("::starthub:state::") {
                let json_part = &line[18..]; // Remove "::starthub:state::" prefix
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_part) {
                    final_output = parsed;
                    break;
                }
            }
        }
        
        Ok(final_output)
    } else {
        println!("❌ WASM execution failed with exit code: {}", result.status);
        Err(anyhow::anyhow!("WASM execution failed: {}", stderr))
    }
}

/// Build final outputs based on export configuration
fn build_final_outputs(
    _export: &serde_json::Value,
    step_outputs: &std::collections::HashMap<String, serde_json::Value>
) -> Result<serde_json::Value> {
    // For now, we'll return the step outputs as the final outputs
    // TODO: Implement proper export mapping based on export configuration
    let mut final_outputs = serde_json::Map::new();
    
    for (step_id, step_output) in step_outputs {
        final_outputs.insert(step_id.clone(), step_output.clone());
    }
    
    Ok(serde_json::Value::Object(final_outputs))
}

/// Download a WASM artifact from the given URL (handles ZIP files)
async fn download_wasm_artifact(url: &str, output_path: &std::path::Path) -> Result<()> {
    println!("📥 Downloading artifact from: {}", url);
    
    let response = reqwest::get(url).await?;
    if !response.status().is_success() {
        return Err(anyhow::anyhow!("Failed to download artifact: HTTP {}", response.status()));
    }
    
    let bytes = response.bytes().await?;
    
    // Create parent directory if it doesn't exist
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    
    // Check if the URL points to a ZIP file (artifact.zip)
    if url.ends_with("artifact.zip") {
        // Extract WASM file from ZIP
        let cursor = std::io::Cursor::new(&bytes);
        let mut archive = zip::ZipArchive::new(cursor)?;
        
        // Find the WASM file in the archive
        let mut wasm_file = None;
        for i in 0..archive.len() {
            let file = archive.by_index(i)?;
            if file.name().ends_with(".wasm") {
                wasm_file = Some(i);
                break;
            }
        }
        
        if let Some(file_index) = wasm_file {
            let mut wasm_file = archive.by_index(file_index)?;
            let mut wasm_content = Vec::new();
            std::io::copy(&mut wasm_file, &mut wasm_content)?;
            std::fs::write(output_path, wasm_content)?;
            println!("✅ Extracted WASM file from ZIP to: {}", output_path.display());
        } else {
            return Err(anyhow::anyhow!("No WASM file found in ZIP archive"));
        }
    } else {
        // Direct WASM file download (legacy support)
        std::fs::write(output_path, bytes)?;
        println!("✅ Downloaded WASM file to: {}", output_path.display());
    }
    
    Ok(())
}

// safe write with overwrite prompt
pub fn write_file_guarded(path: &Path, contents: &str) -> anyhow::Result<()> {
    if path.exists() {
        let overwrite = Confirm::new(&format!("{} exists. Overwrite?", path.display()))
            .with_default(false)
            .prompt()?;
        if !overwrite { return Ok(()); }
    }
    fs::write(path, contents)?;
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> anyhow::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> anyhow::Result<()> { Ok(()) }




pub async fn cmd_publish_docker_inner(m: &ShManifest, no_build: bool) -> anyhow::Result<()> {
    let tag = format!("{}:{}", m.name, m.version);

    if !no_build {
        run("docker", &["build", "-t", &tag, "."])?;
    }

    // Save Docker image as tar file
    let tar_filename = format!("{}-{}.tar", m.name, m.version);
    run("docker", &["save", "-o", &tar_filename, &tag])?;

    // Compress the tar file
    let zip_filename = format!("{}-{}.zip", m.name, m.version);
    run("zip", &["-j", &zip_filename, &tar_filename])?;

    // Get the user's namespace from their profile
    let namespace = match get_user_namespace().await {
        Ok(Some(ns)) => ns,
        Ok(None) => {
            println!("⚠️  No authentication found. Using default namespace 'actions'");
            println!("💡 Use 'starthub login' to authenticate and use your personal namespace");
            "actions".to_string()
        }
        Err(e) => {
            println!("⚠️  Failed to get user namespace: {}. Using default namespace 'actions'", e);
            println!("💡 Use 'starthub login' to authenticate and use your personal namespace");
            "actions".to_string()
        }
    };
    
    println!("🏷️  Using namespace: {}", namespace);
    
    // Upload to Supabase storage with new path structure: <namespace>/<name>/<version>
    let storage_url = format!(
        "{}/storage/v1/object/public/artifacts/{}/{}/{}/artifact.zip",
        crate::config::STARTHUB_API_BASE, namespace, m.name, m.version
    );
    
    // Upload to Supabase Storage using AWS SDK
    println!("📤 Uploading to Supabase Storage using AWS SDK");
    
    // Get file size for verification
    let metadata = fs::metadata(&zip_filename)?;
    println!("📁 File size: {} bytes", metadata.len());
    
    // Use the artifacts bucket directly as specified in the URL
    let bucket_name = "artifacts";
    let object_key = format!("{}/{}/{}/artifact.zip", namespace, m.name, m.version);
    
    println!("🪣 Bucket: {}", bucket_name);
    println!("🔑 Object key: {}", object_key);
    
    // Read the zip file
    let zip_data = fs::read(&zip_filename)?;
    
    // Upload using AWS SDK to Supabase Storage S3 endpoint
    println!("🔄 Uploading to Supabase Storage using AWS SDK...");

    // Set AWS credentials environment variables for Supabase Storage S3 compatibility
    std::env::set_var("AWS_ACCESS_KEY_ID", crate::config::S3_ACCESS_KEY);
    std::env::set_var("AWS_SECRET_ACCESS_KEY", crate::config::S3_SECRET_KEY);
    
    // Configure AWS SDK for Supabase Storage S3 compatibility
    let region_provider = RegionProviderChain::first_try(Region::new(crate::config::SUPABASE_STORAGE_REGION));
    let shared_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(region_provider)
        .load()
        .await;
    
    // Create S3 client with custom endpoint
    // Ensure the endpoint ends with a slash for proper URL construction
    let endpoint_url = if crate::config::SUPABASE_STORAGE_S3_ENDPOINT.ends_with('/') {
        crate::config::SUPABASE_STORAGE_S3_ENDPOINT.to_string()
    } else {
        format!("{}/", crate::config::SUPABASE_STORAGE_S3_ENDPOINT)
    };
    
    let s3_config = aws_sdk_s3::config::Builder::from(&shared_config)
        .endpoint_url(&endpoint_url)
        .force_path_style(true) // Use path-style for Supabase Storage S3 compatibility
        .build();
    
    println!("🔗 AWS SDK S3 endpoint: {}", crate::config::SUPABASE_STORAGE_S3_ENDPOINT);
    println!("🪣 Bucket: {}", bucket_name);
    println!("🔑 Object key: {}", object_key);
    
    let s3_client = S3Client::from_conf(s3_config);

    // Create ByteStream from the zip data
    let body = ByteStream::from(zip_data.clone());

    // Upload using AWS SDK
    let put_object_output = s3_client
        .put_object()
        .bucket(bucket_name)
        .key(&object_key)
        .body(body)
        .content_type("application/zip")
        .send()
        .await;

    match put_object_output {
        Ok(_) => {
            println!("✅ Successfully uploaded to Supabase Storage using AWS SDK");
        }
        Err(e) => {
            println!("❌ Upload failed: {:?}", e);
            anyhow::bail!("Failed to upload to Supabase Storage");
        }
    }
    
    // Clean up temporary files
    fs::remove_file(&tar_filename)?;
    fs::remove_file(&zip_filename)?;

    // Generate a digest for the uploaded artifact
    let digest = format!("sha256:{}", m.name); // Simplified digest for now
    
    // Use the storage_url directly as it already includes the full path to artifact.zip
    let primary = storage_url.clone();
    let lock = ShLock {
        name: m.name.clone(),
        description: m.description.clone(),
        version: m.version.clone(),
        kind: m.kind.clone().expect("Kind should be present in manifest"),
        manifest_version: m.manifest_version,
        repository: m.repository.clone(),
        image: m.image.clone(),
        license: m.license.clone(),
        inputs: m.inputs.clone(),
        outputs: m.outputs.clone(),
        types: m.types.clone(),
        distribution: ShDistribution { primary, upstream: None },
        digest,
        composition: if m.kind == Some(crate::models::ShKind::Composition) {
            Some(m.clone())
        } else {
            None
        },
    };
    
    // Write lock file locally
    fs::write("starthub.lock.json", serde_json::to_string_pretty(&lock)?)?;
    
    // Now update the database with action and version information
    println!("🗄️  Updating database with action information...");
    update_action_database(&m, &namespace).await?;
    
    // Upload lock file to the same Supabase Storage location
    println!("📤 Uploading lock file to Supabase Storage...");
    
    let lock_data = serde_json::to_string_pretty(&lock)?.into_bytes();
    let lock_object_key = format!("{}/{}/{}/lock.json", namespace, m.name, m.version);
    
    println!("🔑 Lock file object key: {}", lock_object_key);
    
    // Create ByteStream from the lock file data
    let lock_body = ByteStream::from(lock_data);
    
    // Upload lock file using AWS SDK
    let lock_put_object_output = s3_client
        .put_object()
        .bucket(bucket_name)
        .key(&lock_object_key)
        .body(lock_body)
        .content_type("application/json")
        .send()
        .await;
    
    match lock_put_object_output {
        Ok(_) => {
            println!("✅ Successfully uploaded lock file to Supabase Storage");
        }
        Err(e) => {
            println!("❌ Lock file upload failed: {:?}", e);
            anyhow::bail!("Failed to upload lock file to Supabase Storage");
        }
    }
    
    println!("✅ Docker image and lock file published to Supabase storage");
    println!("🔗 Storage URL: {}", storage_url);
    println!("🔗 Lock file URL: {}/storage/v1/object/public/{}/{}", 
        crate::config::STARTHUB_API_BASE, bucket_name, lock_object_key);
    Ok(())
}


pub fn find_wasm_artifact(crate_name: &str) -> Option<String> {
    use std::ffi::OsStr;

    let name_dash = crate_name.to_string();
    let name_underscore = crate_name.replace('-', "_");

    let candidate_dirs = [
        "target/wasm32-wasi/release",
        "target/wasm32-wasi/release/deps",
        "target/wasm32-wasip1/release",
        "target/wasm32-wasip1/release/deps",
    ];

    // 1) Try exact filenames first (dash & underscore)
    for dir in &candidate_dirs {
        for fname in [
            format!("{}/{}.wasm", dir, name_dash),
            format!("{}/{}.wasm", dir, name_underscore),
        ] {
            if Path::new(&fname).exists() {
                return Some(fname);
            }
        }
    }

    // 2) Fallback: pick the newest *.wasm in the candidate dirs
    let mut newest: Option<(std::time::SystemTime, String)> = None;
    for dir in &candidate_dirs {
        if let Ok(rd) = fs::read_dir(dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                if path.extension() == Some(OsStr::new("wasm")) {
                    if let Ok(meta) = entry.metadata() {
                        if let Ok(modified) = meta.modified() {
                            let pstr = path.to_string_lossy().to_string();
                            // Prefer files that contain the crate name (dash or underscore)
                            let contains_name = pstr.contains(&name_dash) || pstr.contains(&name_underscore);
                            let score_time = (modified, pstr.clone());
                            match &mut newest {
                                None => newest = Some(score_time),
                                Some((t, _)) if modified > *t => newest = Some(score_time),
                                _ => {}
                            }
                            // If it's clearly our crate, short-circuit
                            if contains_name {
                                return Some(pstr);
                            }
                        }
                    }
                }
            }
        }
    }
    newest.map(|(_, p)| p)
}




pub async fn cmd_publish_wasm_inner(m: &ShManifest, no_build: bool) -> anyhow::Result<()> {
    // WASM PUBLISHING FUNCTION - This is the WASM-specific implementation
    // Get the user's namespace from their profile
    let namespace = match get_user_namespace().await {
        Ok(Some(ns)) => ns,
        Ok(None) => {
            println!("⚠️  No authentication found. Using default namespace 'actions'");
            println!("💡 Use 'starthub login' to authenticate and use your personal namespace");
            "actions".to_string()
        }
        Err(e) => {
            println!("⚠️  Failed to get user namespace: {}. Using default namespace 'actions'", e);
            println!("💡 Use 'starthub login' to authenticate and use your personal namespace");
            "actions".to_string()
        }
    };
    
    println!("🏷️  Using namespace: {}", namespace);

    if !no_build {
        // Try cargo-component (component model) first; fall back to plain WASI target.
        // Ignore rustup failure if target already installed.
        let _ = run("rustup", &["target", "add", "wasm32-wasip1"]);
        // Prefer cargo-component if available
        if run("cargo", &["+nightly", "component", "--version"]).is_ok() {
            run("cargo", &["+nightly", "component", "build", "--release"])?;
        } else {
            run("cargo", &["build", "--release", "--target", "wasm32-wasip1"])?;
        }
    }

    // Find the .wasm produced by the build
    let wasm_path = find_wasm_artifact(&m.name)
        .ok_or_else(|| anyhow::anyhow!("WASM build artifact not found; looked in `target/**/release/**/*.wasm`"))?;

    // Create a zip file containing the WASM artifact
    let zip_filename = format!("{}-{}.zip", m.name, m.version);
    run("zip", &["-j", &zip_filename, &wasm_path])?;
    
    // Upload to Supabase Storage using AWS SDK
    println!("📤 Uploading to Supabase Storage using AWS SDK");
    
    // Get file size for verification
    let metadata = fs::metadata(&zip_filename)?;
    println!("📁 File size: {} bytes", metadata.len());
    
    // Use the same bucket as Docker publishing since it's working
    let bucket_name = "artifacts";
    let object_key = format!("{}/{}/{}/artifact.zip", namespace, m.name, m.version);
    
    println!("🪣 Bucket: {}", bucket_name);
    println!("🔑 Object key: {}", object_key);
    
    // Read the zip file
    let zip_data = fs::read(&zip_filename)?;
    
    // Upload using AWS SDK to Supabase Storage S3 endpoint
    println!("🔄 Uploading to Supabase Storage using AWS SDK...");

    // Set AWS credentials environment variables for Supabase Storage S3 compatibility
    std::env::set_var("AWS_ACCESS_KEY_ID", crate::config::S3_ACCESS_KEY);
    std::env::set_var("AWS_SECRET_ACCESS_KEY", crate::config::S3_SECRET_KEY);

    // Configure AWS SDK for Supabase Storage S3 compatibility
    let region_provider = RegionProviderChain::first_try(Region::new(crate::config::SUPABASE_STORAGE_REGION));
    let shared_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(region_provider)
        .load()
        .await;
    
    // Create S3 client with custom endpoint
    // Ensure the endpoint ends with a slash for proper URL construction
    let endpoint_url = if crate::config::SUPABASE_STORAGE_S3_ENDPOINT.ends_with('/') {
        crate::config::SUPABASE_STORAGE_S3_ENDPOINT.to_string()
    } else {
        format!("{}/", crate::config::SUPABASE_STORAGE_S3_ENDPOINT)
    };
    
    let s3_config = aws_sdk_s3::config::Builder::from(&shared_config)
        .endpoint_url(&endpoint_url)
        .force_path_style(true) // Use path-style for Supabase Storage S3 compatibility
        .build();
    
    println!("🔗 AWS SDK S3 endpoint: {}", crate::config::SUPABASE_STORAGE_S3_ENDPOINT);
    println!("🪣 Bucket: {}", bucket_name);
    println!("🔑 Object key: {}", object_key);
    
    let s3_client = S3Client::from_conf(s3_config);

    // Create ByteStream from the zip file data
    let body = ByteStream::from(zip_data);

    // Upload the zip file
    let put_object_output = s3_client
        .put_object()
        .bucket(bucket_name)
        .key(&object_key)
        .body(body)
        .content_type("application/zip")
        .send()
        .await;

    match put_object_output {
        Ok(_) => {
            println!("✅ Successfully uploaded WASM artifact to Supabase Storage");
        }
        Err(e) => {
            println!("❌ Upload failed: {:?}", e);
            anyhow::bail!("Failed to upload WASM artifact to Supabase Storage");
        }
    }

    // Create lock file with the same structure as Docker
    let digest = format!("sha256:{}", m.name); // Simplified digest for now
    
    // Construct the public URL for the artifact.zip file
    let primary = format!(
        "{}/storage/v1/object/public/artifacts/{}/{}/{}/artifact.zip",
        crate::config::STARTHUB_API_BASE, namespace, m.name, m.version
    );
    let lock = ShLock {
        name: m.name.clone(),
        description: m.description.clone(),
        version: m.version.clone(),
        kind: m.kind.clone().expect("Kind should be present in manifest"),
        manifest_version: m.manifest_version,
        repository: m.repository.clone(),
        image: m.image.clone(),
        license: m.license.clone(),
        inputs: m.inputs.clone(),
        outputs: m.outputs.clone(),
        types: m.types.clone(),
        distribution: ShDistribution { primary, upstream: None },
        digest,
        composition: if m.kind == Some(crate::models::ShKind::Composition) {
            Some(m.clone())
        } else {
            None
        },
    };
    
    // Write lock file locally
    fs::write("starthub.lock.json", serde_json::to_string_pretty(&lock)?)?;
    
    // Upload lock file to the same Supabase Storage location
    println!("📤 Uploading lock file to Supabase Storage...");
    
    let lock_data = serde_json::to_string_pretty(&lock)?.into_bytes();
    let lock_object_key = format!("{}/{}/{}/lock.json", namespace, m.name, m.version);
    
    println!("🔑 Lock file object key: {}", lock_object_key);
    
    // Create ByteStream from the lock file data
    let lock_body = ByteStream::from(lock_data);
    
    // Upload lock file using AWS SDK
    let lock_put_object_output = s3_client
        .put_object()
        .bucket(bucket_name)
        .key(&lock_object_key)
        .body(lock_body)
        .content_type("application/json")
        .send()
        .await;
    
    match lock_put_object_output {
        Ok(_) => {
            println!("✅ Successfully uploaded lock file to Supabase Storage");
        }
        Err(e) => {
            println!("❌ Lock file upload failed: {:?}", e);
            anyhow::bail!("Failed to upload lock file to Supabase Storage");
        }
    }
    
    println!("✅ WASM artifact and lock file published to Supabase storage");
    println!("🔗 Storage URL: {}", endpoint_url);
    println!("🔗 Lock file URL: {}/storage/v1/object/public/{}/{}",
        crate::config::STARTHUB_API_BASE, bucket_name, lock_object_key);
    
    // Now update the database with action and version information
    println!("🗄️  Updating database with action information...");
    update_action_database(&m, &namespace).await?;
    
    // Clean up local files
    fs::remove_file(&zip_filename)?;
    
    Ok(())
}

pub fn run(cmd: &str, args: &[&str]) -> anyhow::Result<()> {
    match PCommand::new(cmd).args(args).status() {
        Ok(status) => {
            anyhow::ensure!(status.success(), "command failed: {} {:?}", cmd, args);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("`{}` not found. Install it first (e.g., `brew install {}`)", cmd, cmd)
        }
        Err(e) => Err(e.into()),
    }
}



// ------------------- cmd_init -------------------
pub async fn cmd_init(path: String) -> anyhow::Result<()> {
    // Basic fields
    let name = Text::new("Package name:")
        .with_default("http-get-wasm")
        .prompt()?;

    let version = Text::new("Version:")
        .with_default("0.0.1")
        .prompt()?;

    // Kind
    let kind_str = Select::new("Kind:", vec!["wasm", "docker"]).prompt()?;
    let kind = match kind_str {
        "wasm" => ShKind::Wasm,
        "docker" => ShKind::Docker,
        // "composition" => ShKind::Composition,
        _ => unreachable!(),
    };

    // Repository
    let repo_default = match kind {
        ShKind::Wasm   => "github.com/starthubhq/http-get-wasm",
        ShKind::Docker => "github.com/starthubhq/http-get-wasm",
        ShKind::Composition => "github.com/starthubhq/composite-action",
    };
    let repository = Text::new("Repository:")
        .with_help_message("Git repository URL (e.g., github.com/org/repo)")
        .with_default(repo_default)
        .prompt()?;

    // Since we're using Supabase Storage instead of OCI registries, 
    // we don't need to prompt for OCI image paths anymore
    let _image: Option<String> = None;

    // License
    let license = Select::new("License:", vec![
        "Apache-2.0", "MIT", "BSD-3-Clause", "GPL-3.0", "Unlicense", "Proprietary",
    ]).prompt()?.to_string();

    // Example I/O ports to show users what they should look like
    let inputs: Vec<ShPort> = vec![
        ShPort {
            name: "url".to_string(),
            description: "The URL to fetch data from".to_string(),
            ty: crate::models::ShType::String,
            required: true,
            default: None,
        }
    ];
    let outputs: Vec<ShPort> = vec![
        ShPort {
            name: "response".to_string(),
            description: "The HTTP response data".to_string(),
            ty: crate::models::ShType::Object,
            required: true,
            default: None,
        }
    ];

    // Manifest
    let manifest = ShManifest { 
        name: name.clone(), 
        description: "Generated manifest".to_string(),
        version: version.clone(), 
        manifest_version: 1,
        kind: Some(kind.clone()), 
        repository: repository.clone(), 
        image: None, // No OCI image needed with Supabase Storage
        license: license.clone(), 
        inputs, 
        outputs,
        types: std::collections::HashMap::new(),
        steps: vec![],
        wires: vec![],
        export: serde_json::json!({}),
    };
    let json = serde_json::to_string_pretty(&manifest)?;

    // Ensure dir
    let out_dir = Path::new(&path);
    if !out_dir.exists() {
        fs::create_dir_all(out_dir)?;
    }

    // Write starthub.json
    write_file_guarded(&out_dir.join("starthub.json"), &json)?;
    // Always create .gitignore / .dockerignore / README.md
    write_file_guarded(&out_dir.join(".gitignore"), templates::GITIGNORE_TPL)?;
    // .dockerignore only for Docker projects
    if matches!(kind, ShKind::Docker) {
        write_file_guarded(&out_dir.join(".dockerignore"), templates::DOCKERIGNORE_TPL)?;
    }
    let readme = templates::readme_tpl(&name, &kind, &repository, &license);
    write_file_guarded(&out_dir.join("README.md"), &readme)?;

    // If docker, scaffold Dockerfile + entrypoint.sh
    if matches!(kind, ShKind::Docker) {
        let dockerfile = out_dir.join("Dockerfile");
        write_file_guarded(&dockerfile, templates::DOCKERFILE_TPL)?;
        let entrypoint = out_dir.join("entrypoint.sh");
        write_file_guarded(&entrypoint, templates::ENTRYPOINT_SH_TPL)?;
        make_executable(&entrypoint)?;
    }

    // If wasm, scaffold Cargo.toml + src/main.rs
    if matches!(kind, ShKind::Wasm) {
        let cargo = out_dir.join("Cargo.toml");
        write_file_guarded(&cargo, &templates::wasm_cargo_toml_tpl(&name, &version))?;

        let src_dir = out_dir.join("src");
        if !src_dir.exists() {
            fs::create_dir_all(&src_dir)?;
        }
        let main_rs = src_dir.join("main.rs");
        write_file_guarded(&main_rs, templates::WASM_MAIN_RS_TPL)?;
    }

    println!("✓ Wrote {}", out_dir.join("starthub.json").display());
    Ok(())
}


/// Authenticate with Starthub backend using browser-based flow
pub async fn cmd_login_starthub(api_base: String) -> anyhow::Result<()> {
    println!("🔐 Authenticating with Starthub backend...");
    println!("🌐 API Base: {}", api_base);
    
    // Open browser to editor for authentication
    let editor_url = "https://editor.starthub.so/cli-auth";
    println!("🌐 Opening browser to: {}", editor_url);
    
    match webbrowser::open(editor_url) {
        Ok(_) => println!("✅ Browser opened successfully"),
        Err(e) => println!("⚠️  Could not open browser automatically: {}", e),
    }
    
    println!("\n📋 Please:");
    println!("1. Wait for the authentication code to appear in your browser");
    println!("2. Copy the authentication code from the browser");
    println!("3. Come back here and paste the code below");
    
    // Wait for user to paste the code
    let pasted_code = inquire::Text::new("Paste the authentication code:")
        .with_help_message("Enter the code from your browser")
        .prompt()?;
    
    // Validate the code against the backend
    println!("🔄 Validating authentication code...");
    
    let client = reqwest::Client::new();
    let validation_response = client
        .post(&format!("{}/functions/v1/cli-auth", api_base))
        .json(&serde_json::json!({
            "code": pasted_code
        }))
        .send()
        .await?;
    
    let status = validation_response.status();
    if !status.is_success() {
        let error_text = validation_response.text().await?;
        anyhow::bail!("Code validation failed: {} ({})", status, error_text);
    }
    
    let validation_data: serde_json::Value = validation_response.json().await?;
    
    if !validation_data.get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false) {
        let error_msg = validation_data.get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        anyhow::bail!("Authentication failed: {}", error_msg);
    }
    
    let profile = validation_data.get("profile")
        .ok_or_else(|| anyhow::anyhow!("No profile data in response"))?;
    
    let email = profile.get("email")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("No email in profile"))?;
    
    let access_token = validation_data.get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("No access token in response"))?;
    
    // Store the authentication info
    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?
        .join("starthub");
    
    fs::create_dir_all(&config_dir)?;
    
    let config_file = config_dir.join("auth.json");
    let auth_config = serde_json::json!({
        "api_base": api_base,
        "email": email,
        "profile_id": profile.get("id").and_then(|v| v.as_str()).unwrap_or(""),
        "username": profile.get("username").and_then(|v| v.as_str()).unwrap_or(""),
        "full_name": profile.get("full_name").and_then(|v| v.as_str()).unwrap_or(""),
        "namespace": profile.get("username").and_then(|v| v.as_str()).unwrap_or(""), // Use username as namespace for now
        "access_token": access_token,
        "login_time": chrono::Utc::now().to_rfc3339(),
        "auth_method": "cli_code"
    });
    
    fs::write(&config_file, serde_json::to_string_pretty(&auth_config)?)?;
    
    println!("✅ Authentication successful!");
    println!("🔑 Authentication data saved to: {}", config_file.display());
    println!("📧 Logged in as: {}", email);
    
    Ok(())
}



/// Load stored authentication configuration
pub fn load_auth_config() -> anyhow::Result<Option<(String, String, String, String, String)>> {
    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?
        .join("starthub");
    
    let config_file = config_dir.join("auth.json");
    
    if !config_file.exists() {
        return Ok(None);
    }
    
    let config_content = fs::read_to_string(&config_file)?;
    let auth_config: serde_json::Value = serde_json::from_str(&config_content)?;
    
    let api_base = auth_config.get("api_base")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("No api_base in auth config"))?;
    
    let email = auth_config.get("email")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("No email in auth config"))?;
    
    let profile_id = auth_config.get("profile_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("No profile_id in auth config"))?;
    
    let namespace = auth_config.get("namespace")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("No namespace in auth config"))?;
    
    let access_token = auth_config.get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("No access_token in auth config"))?;
    
    Ok(Some((api_base.to_string(), email.to_string(), profile_id.to_string(), namespace.to_string(), access_token.to_string())))
}

/// Logout from Starthub backend
pub async fn cmd_logout_starthub() -> anyhow::Result<()> {
    println!("🔓 Logging out from Starthub backend...");
    
    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?
        .join("starthub");
    
    let config_file = config_dir.join("auth.json");
    
    if !config_file.exists() {
        println!("ℹ️  No authentication found. Already logged out.");
        return Ok(());
    }
    
    // Remove the auth file
    fs::remove_file(&config_file)?;
    
    println!("✅ Successfully logged out!");
    println!("🗑️  Authentication data removed from: {}", config_file.display());
    
    Ok(())
}

/// Get the namespace for the currently authenticated user
pub async fn get_user_namespace() -> anyhow::Result<Option<String>> {
    // Load authentication config
    let auth_config = match load_auth_config()? {
        Some((api_base, _email, profile_id, _namespace, _access_token)) => (api_base, profile_id),
        None => return Ok(None),
    };
    
    let (api_base, profile_id) = auth_config;
    
    // Query the owners table directly using PostgREST
    let client = reqwest::Client::new();
    let response = client
        .get(&format!("{}/rest/v1/owners?select=namespace&owner_type=eq.PROFILE&profile_id=eq.{}", api_base, profile_id))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", profile_id))
        .send()
        .await?;
    
    if response.status().is_success() {
        let data: Vec<serde_json::Value> = response.json().await?;
        if let Some(owner) = data.first() {
            if let Some(namespace) = owner.get("namespace").and_then(|v| v.as_str()) {
                return Ok(Some(namespace.to_string()));
            }
        }
    }
    
    // Fallback: try to get from local auth config username
    let auth_config = load_auth_config()?;
    if let Some((_api_base, _email, _profile_id, namespace, _access_token)) = auth_config {
        // Use the locally stored namespace as fallback
        return Ok(Some(namespace));
    }
    
    Ok(None)
}

/// Show current authentication status
pub async fn cmd_auth_status() -> anyhow::Result<()> {
    println!("🔍 Checking authentication status...");
    
    match load_auth_config()? {
        Some((api_base, email, profile_id, namespace, access_token)) => {
            println!("✅ Authenticated with Starthub backend");
            println!("🌐 API Base: {}", api_base);
            println!("📧 Email: {}", email);
            println!("🆔 Profile ID: {}", profile_id);
            println!("🏷️  Namespace: {}", namespace);
            
            // Try to validate the authentication by making a test API call
            println!("🔄 Validating authentication...");
            let client = reqwest::Client::new();
            let response = client
                .get(&format!("{}/functions/v1/profiles", api_base))
                .header("Authorization", format!("Bearer {}", access_token))
                .send()
                .await?;
            
            if response.status().is_success() {
                println!("✅ Authentication is valid and working");
            } else {
                println!("⚠️  Authentication may be expired or invalid");
            }
        }
        None => {
            println!("❌ Not authenticated");
            println!("💡 Use 'starthub login' to authenticate");
        }
    }
    
    Ok(())
}

pub async fn cmd_run(action: String, _runner: crate::RunnerKind) -> Result<()> {
    // Start the server as a separate process
    let server_process = start_server_process().await?;
    
    // Wait a moment for server to start
    sleep(Duration::from_millis(1000)).await;
    
    // Parse the action argument to extract namespace, slug, and version
    let (namespace, slug, version) = parse_action_arg(&action);
    
    // Open browser to the server with a proper route for the Vue app
    let url = format!("{}/{}/{}/{}", LOCAL_SERVER_URL, namespace, slug, version);
    match webbrowser::open(&url) {
        Ok(_) => println!("↗ Opened browser to: {url}"),
        Err(e) => println!("→ Browser: {url} (couldn't auto-open: {e})"),
    }
    
    println!("🚀 Server started at {}", LOCAL_SERVER_URL);
    println!("📱 Serving UI for action: {} at route: {}", action, url);
    println!("🔄 Press Ctrl+C to stop the server");
    
    // Wait for Ctrl+C signal
    tokio::signal::ctrl_c().await?;
    println!("\n🛑 Shutting down server...");
    
    // Kill the server process
    if let Some(mut child) = server_process {
        let _ = child.kill().await;
        println!("✅ Server process terminated");
    }
    
    Ok(())
}

async fn start_server_process() -> Result<Option<tokio::process::Child>> {
    // Try to find the starthub-server binary
    let server_binary = if cfg!(target_os = "windows") {
        "starthub-server.exe"
    } else {
        "starthub-server"
    };
    
    // First try to find it in the current directory or PATH
    let server_path = which::which(server_binary)
        .or_else(|_| {
            // Try relative to the current binary
            let current_exe = std::env::current_exe()?;
            let current_dir = current_exe.parent().unwrap();
            Ok(current_dir.join(server_binary))
        })
        .or_else(|_| {
            // Try in the target directory for development
            Ok(std::env::current_dir()?.join("target").join("debug").join(server_binary))
        })?;
    
    if !server_path.exists() {
        return Err(anyhow::anyhow!(
            "Server binary not found. Please build the server first with: cargo build --bin starthub-server"
        ));
    }
    
    println!("🚀 Starting server process: {:?}", server_path);
    
    // Start the server process
    let mut child = tokio::process::Command::new(&server_path)
        .arg("--bind")
        .arg(LOCAL_SERVER_HOST)
        .spawn()?;
    
    Ok(Some(child))
}

// Parse action argument in format "namespace/slug@version" or "namespace/slug"
fn parse_action_arg(action: &str) -> (String, String, String) {
    // Default values
    let mut namespace = "tgirotto".to_string();
    let mut slug = "test-action".to_string();
    let mut version = "0.1.0".to_string();
    
    if action.contains('/') {
        let parts: Vec<&str> = action.split('/').collect();
        if parts.len() >= 2 {
            namespace = parts[0].to_string();
            let full_slug = parts[1].to_string();
            
            // Check if slug contains version (e.g., "test-action@0.1.0" or "test-action:0.1.0")
            if full_slug.contains('@') {
                let slug_parts: Vec<&str> = full_slug.split('@').collect();
                if slug_parts.len() >= 2 {
                    slug = slug_parts[0].to_string();
                    version = slug_parts[1].to_string();
                }
            } else if full_slug.contains(':') {
                let slug_parts: Vec<&str> = full_slug.split(':').collect();
                if slug_parts.len() >= 2 {
                    slug = slug_parts[0].to_string();
                    version = slug_parts[1].to_string();
                }
            } else {
                slug = full_slug;
            }
        }
    } else if action.contains('@') {
        // Handle case like "test-action@0.1.0"
        let parts: Vec<&str> = action.split('@').collect();
        if parts.len() >= 2 {
            slug = parts[0].to_string();
            version = parts[1].to_string();
        }
    } else if action.contains(':') {
        // Handle case like "test-action:0.1.0"
        let parts: Vec<&str> = action.split(':').collect();
        if parts.len() >= 2 {
            slug = parts[0].to_string();
            version = parts[1].to_string();
        }
    } else {
        // Just a slug, use defaults for namespace and version
        slug = action.to_string();
    }
    
    (namespace, slug, version)
}

async fn start_server() -> Result<()> {
    // Create shared state
    let state = AppState::new();
    
    // Create router with UI routes and API endpoints
    let app = Router::new()
        .route("/api/status", get(get_status))
        .route("/api/action", post(handle_action))
        .route("/api/run", post(handle_run))
        .route("/api/types", get(get_types))
        .route("/api/types/:action", get(get_types_for_action))
        .route("/api/execution-orders", get(get_execution_orders))
        .route("/api/execution-orders/:action", get(get_execution_order_for_action))
        .route("/ws", get(ws_handler)) // WebSocket endpoint
        .nest_service("/assets", ServeDir::new("ui/dist/assets"))
        .nest_service("/favicon.ico", ServeDir::new("ui/dist"))
        .route("/", get(serve_index))
        .fallback(serve_spa) // SPA fallback for Vue Router
        .layer(CorsLayer::permissive())
        .with_state(state);

    // Start server
    let listener = TcpListener::bind(LOCAL_SERVER_HOST).await?;
    println!("🌐 Server listening on {}", LOCAL_SERVER_URL);
    
    axum::serve(listener, app).await?;
    Ok(())
}

async fn serve_index() -> Html<String> {
    // Read and serve the index.html file
    match fs::read_to_string("ui/dist/index.html") {
        Ok(content) => Html(content),
        Err(_) => Html("<!DOCTYPE html><html><body><h1>UI not found</h1><p>Make sure to build the UI first</p></body></html>".to_string())
    }
}

// SPA fallback - serve index.html for all routes to support Vue Router
async fn serve_spa() -> Html<String> {
    match fs::read_to_string("ui/dist/index.html") {
        Ok(content) => Html(content),
        Err(_) => Html("<!DOCTYPE html><html><body><h1>UI not found</h1><p>Make sure to build the UI first</p></body></html>".to_string())
    }
}

async fn get_status() -> Json<Value> {
    Json(json!({
        "status": "running",
        "timestamp": chrono::Utc::now().to_rfc3339()
    }))
}

async fn get_types(
    axum::extract::State(state): axum::extract::State<AppState>
) -> Json<Value> {
    let all_types = state.get_all_types();
    Json(json!({
        "success": true,
        "types": all_types,
        "count": all_types.len()
    }))
}

async fn get_types_for_action(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(action): axum::extract::Path<String>
) -> Json<Value> {
    let types = state.get_types_for_action(&action);
    Json(json!({
        "success": true,
        "action": action,
        "types": types,
        "count": types.len()
    }))
}

async fn get_execution_orders(
    axum::extract::State(state): axum::extract::State<AppState>
) -> Json<Value> {
    let all_orders = state.get_all_execution_orders();
    Json(json!({
        "success": true,
        "execution_orders": all_orders,
        "count": all_orders.len()
    }))
}

async fn get_execution_order_for_action(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(action): axum::extract::Path<String>
) -> Json<Value> {
    match state.get_execution_order(&action) {
        Some(order) => Json(json!({
            "success": true,
            "action": action,
            "execution_order": order,
            "count": order.len()
        })),
        None => Json(json!({
            "success": false,
            "action": action,
            "error": "No execution order found for this action"
        }))
    }
}

async fn handle_action(Json(payload): Json<Value>) -> Json<Value> {
    // Handle action requests from the UI
    Json(json!({
        "success": true,
        "message": "Action received",
        "data": payload
    }))
}

async fn handle_run(
    axum::extract::State(state): axum::extract::State<AppState>,
    Json(payload): Json<Value>
) -> Json<Value> {
    // Handle the /api/run endpoint that InputsComponent expects
    println!("🚀 Received run request: {:?}", payload);
    
    // Extract action and inputs from payload
    let action = payload.get("action").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
    let default_inputs = json!([]);
    let inputs_value = payload.get("inputs").unwrap_or(&default_inputs);
    
    // Convert inputs array to Vec<Value> for direct use
    let inputs: Vec<serde_json::Value> = if let Some(inputs_array) = inputs_value.as_array() {
        inputs_array.clone()
    } else {
        Vec::new()
    };
    
    println!("📋 Action: {}", action);
    println!("📥 Inputs: {:?}", inputs);
    
    // Initialize client for API calls
    let base = std::env::var("STARTHUB_API").unwrap_or_else(|_| "https://api.starthub.so".to_string());
            let client = crate::starthub_api::Client::new(base, Some(crate::config::SUPABASE_ANON_KEY.to_string()));
    
    println!("🚀 Starting artifact download for action: {}", action);
            
    // Recursively fetch all lock files for the action and its dependencies
            let mut visited = std::collections::HashSet::new();
    match fetch_all_action_locks(&client, &action, &mut visited).await {
        Ok(locks) => {
            if locks.is_empty() {
                println!("❌ No lock files found for action: {}", action);
                        return Json(json!({
                            "success": false,
                    "message": "No lock files found",
                            "action": action,
                    "error": "Unable to find published artifacts for this action"
                        }));
                    }
                    
            println!("✅ Successfully fetched {} lock file(s)", locks.len());
            
            // Create artifacts directory
            let artifacts_dir = std::path::Path::new("./artifacts");
            if let Err(e) = std::fs::create_dir_all(artifacts_dir) {
                println!("❌ Failed to create artifacts directory: {}", e);
                        return Json(json!({
                            "success": false,
                    "message": "Failed to create artifacts directory",
                            "action": action,
                    "error": e.to_string()
                        }));
                    }
                    
            // Store types from all lock files, compute execution orders, and download WASM artifacts
            let mut downloaded_artifacts = Vec::new();
            for lock in &locks {
                println!("📦 Processing lock for: {} v{}", lock.name, lock.version);
                
                let action_ref = format!("{}/{}@{}", 
                    action.split('/').next().unwrap_or("unknown"),
                    lock.name, 
                    lock.version
                );
                
                // Store types from this lock file
                if !lock.types.is_empty() {
                    state.store_types(&action_ref, &lock.types);
                }
                
                // Compute and store execution order for composite actions
                match compute_execution_order(&action_ref, lock).await {
                    Ok(Some(execution_order)) => {
                        state.store_execution_order(&action_ref, execution_order);
                        
                        // Also store the composition data for execution
                        if let Some(composition) = &lock.composition {
                            state.store_composition_data(&action_ref, composition.clone());
                            
                            // Download artifacts for each step in the composition
                            for step in &composition.steps {
                                let step_name = &step.uses.name;
                                let safe_name = step_name.replace('/', "_").replace(':', "_");
                                let artifact_path = artifacts_dir.join(format!("{}.wasm", safe_name));
                                
                                // Construct the artifact URL for this step
                                // step_name is like "http-get-wasm:0.0.15", we need "tgirotto/http-get-wasm/0.0.15"
                                let step_artifact_url = format!(
                                    "https://api.starthub.so/storage/v1/object/public/artifacts/tgirotto/{}/artifact.zip",
                                    step_name.replace(':', "/")
                                );
                                
                                match download_wasm_artifact(&step_artifact_url, &artifact_path).await {
                                    Ok(_) => {
                                        downloaded_artifacts.push(json!({
                                            "name": step_name,
                                            "step_id": step.id,
                                            "kind": "wasm",
                                            "path": artifact_path.to_string_lossy()
                                        }));
                                        println!("✅ Downloaded artifact for step: {} ({})", step.id, step_name);
                                    }
                                    Err(e) => {
                                        println!("⚠️  Failed to download artifact for step {} ({}): {}", step.id, step_name, e);
                                    }
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        // Not a composite action, no execution order needed
                    }
                    Err(e) => {
                        println!("⚠️  Failed to compute execution order for {}: {}", action_ref, e);
                    }
                }
                
                if lock.kind == crate::models::ShKind::Wasm {
                    // Create a safe filename from the action reference
                    let safe_name = action.replace('/', "_").replace('@', "_");
                    let artifact_path = artifacts_dir.join(format!("{}.wasm", safe_name));
                    
                    match download_wasm_artifact(&lock.distribution.primary, &artifact_path).await {
                        Ok(_) => {
                            downloaded_artifacts.push(json!({
                                "name": lock.name,
                                "version": lock.version,
                                "kind": "wasm",
                                "path": artifact_path.to_string_lossy(),
                                "digest": lock.digest
                            }));
                        }
                        Err(e) => {
                            println!("❌ Failed to download artifact for {}: {}", lock.name, e);
                        }
                    }
                } else {
                    println!("ℹ️  Skipping non-WASM artifact: {} (kind: {:?})", lock.name, lock.kind);
                }
            }
            
            // Execute the action - either composite or simple
            let execution_result = if let Some(execution_order) = state.get_execution_order(&action) {
                println!("🔄 Found execution order for composite action: {:?}", execution_order);
                
                // Convert Vec back to HashMap for composite actions
                let inputs_map: std::collections::HashMap<String, serde_json::Value> = inputs.iter()
                    .filter_map(|item| {
                        if let Some(obj) = item.as_object() {
                            obj.iter().next().map(|(k, v)| (k.clone(), v.clone()))
                        } else {
                            None
                        }
                    })
                        .collect();
                    
                match execute_ordered_steps(&action, &execution_order, &inputs_map, &artifacts_dir, &state).await {
                    Ok(outputs) => {
                        println!("✅ Composite action execution completed successfully");
                        Some(outputs)
                    }
                    Err(e) => {
                        println!("❌ Composite action execution failed: {}", e);
                        None
                    }
                }
            } else {
                // Check if this is a composition action that failed to get execution order
                let is_composition = locks.iter().any(|lock| {
                    let action_ref = format!("{}/{}@{}", 
                        action.split('/').next().unwrap_or("unknown"),
                        lock.name, 
                        lock.version
                    );
                    action_ref == action && lock.kind == crate::models::ShKind::Composition
                });
                
                if is_composition {
                    println!("❌ Composition action requires execution order but none was found");
                    println!("💡 This usually means the manifest file could not be fetched or parsed");
                    None
                } else {
                    println!("ℹ️  No execution order found - executing as simple action");
                    
                    // Execute simple WASM action
                    match execute_simple_wasm_action(&action, &inputs, &artifacts_dir).await {
                        Ok(outputs) => {
                            println!("✅ Simple WASM action execution completed successfully");
                            Some(outputs)
                        }
                        Err(e) => {
                            println!("❌ Simple WASM action execution failed: {}", e);
                            None
                        }
                    }
                }
            };
            
            // Send completion message to WebSocket clients
                                let ws_message = json!({
                "type": "artifacts_downloaded",
                                    "action": action,
                "artifacts": downloaded_artifacts,
                "total_downloaded": downloaded_artifacts.len(),
                "execution_result": execution_result
            });
            
                                if let Ok(msg_str) = serde_json::to_string(&ws_message) {
                                    let _ = state.ws_sender.send(msg_str);
                println!("📡 Sent completion message to WebSocket clients");
            }
            
            // Print completion summary
            println!("🎉 Artifact download completed!");
            println!("📊 Summary:");
            println!("   • Total lock files processed: {}", locks.len());
            println!("   • WASM artifacts downloaded: {}", downloaded_artifacts.len());
            println!("   • Artifacts directory: {}", artifacts_dir.display());
            
            for artifact in &downloaded_artifacts {
                println!("   • {} v{} -> {}", 
                    artifact["name"], 
                    artifact["version"], 
                    artifact["path"]
                );
            }
            
                            Json(json!({
                "success": true,
                "message": "Artifacts downloaded successfully",
                                "action": action,
                "artifacts": downloaded_artifacts,
                "total_downloaded": downloaded_artifacts.len(),
                "artifacts_directory": artifacts_dir.to_string_lossy(),
                "execution_result": execution_result
                            }))
                }
                Err(e) => {
            println!("❌ Failed to fetch lock files: {}", e);
                    Json(json!({
                        "success": false,
                "message": "Failed to fetch lock files",
                        "action": action,
                "error": e.to_string()
            }))
        }
    }
}

async fn ws_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    ws: WebSocketUpgrade
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws(socket, state))
}

async fn handle_ws(ws: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = ws.split();

    // Subscribe to the broadcast channel to receive execution plan messages
    let mut ws_receiver = state.ws_sender.subscribe();

    // Send a welcome message
    let welcome_msg = json!({
        "type": "connection",
        "message": "Connected to Starthub WebSocket server",
        "timestamp": chrono::Utc::now().to_rfc3339()
    });
    
    if let Ok(msg) = serde_json::to_string(&welcome_msg) {
        let _ = sender.send(axum::extract::ws::Message::Text(msg)).await;
    }

    // Spawn a task to forward broadcast messages to this WebSocket client
    let sender_arc = std::sync::Arc::new(tokio::sync::Mutex::new(sender));
    let sender_clone = sender_arc.clone();
    let forward_task = tokio::spawn(async move {
        while let Ok(msg) = ws_receiver.recv().await {
            let mut sender_guard = sender_clone.lock().await;
            if let Err(_) = sender_guard.send(axum::extract::ws::Message::Text(msg)).await {
                break; // WebSocket closed
            }
        }
    });

    // Handle incoming messages from the client
    while let Some(msg) = receiver.next().await {
        if let Ok(msg) = msg {
            match msg {
                axum::extract::ws::Message::Text(text) => {
                    // Echo back the message for now
                    let echo_msg = json!({
                        "type": "echo",
                        "message": text,
                        "timestamp": chrono::Utc::now().to_rfc3339()
                    });
                    
                    if let Ok(msg_str) = serde_json::to_string(&echo_msg) {
                        let mut sender_guard = sender_arc.lock().await;
                        let _ = sender_guard.send(axum::extract::ws::Message::Text(msg_str)).await;
                    }
                }
                axum::extract::ws::Message::Close(_) => {
                    break;
                }
                _ => {}
            }
        }
    }

    // Cancel the forward task when the WebSocket closes
    forward_task.abort();
}

pub async fn cmd_status(id: Option<String>) -> Result<()> {
    println!("Status for {id:?}");
    // TODO: poll API
    Ok(())
}

/// Updates the database with action and version information after a successful upload
/// This function:
/// 1. Checks if an action already exists for the given name and namespace
/// 2. Checks if a version already exists for the given action and version number
/// 3. Inserts new action and version if they don't exist
/// 4. Inserts action ports from the lock file
async fn update_action_database(manifest: &ShManifest, namespace: &str) -> anyhow::Result<()> {
    // Load authentication config to get profile_id and API base
    let auth_config = load_auth_config()?;
    let (api_base, _email, profile_id, _namespace, access_token) = auth_config.ok_or_else(|| {
        anyhow::anyhow!("No authentication found in auth config. Please run 'starthub login' first.")
    })?;
    
    println!("Using profile_id from auth config: {}", profile_id);

    // First, check if a git_allowed_repository exists for this namespace, name, and version
    let git_allowed_repository_id = match check_git_allowed_repository_exists(&api_base, namespace, &manifest.name, &manifest.version, &access_token).await? {
        Some(repo_id) => {
            println!("Using existing git_allowed_repository: {}", repo_id);
            Some(repo_id)
        }
        None => {
            // Get the owner ID for creating the git_allowed_repository
            let owner_response = reqwest::Client::new()
                .get(&format!("{}/rest/v1/owners?select=id&namespace=eq.{}", api_base, namespace))
                .header("apikey", crate::config::SUPABASE_ANON_KEY)
                .header("Authorization", format!("Bearer {}", access_token))
                .send()
                .await?;
            
            if !owner_response.status().is_success() {
                anyhow::bail!("Failed to get owner ID: {}", owner_response.status())
            }
            
            let owners: Vec<serde_json::Value> = owner_response.json().await?;
            let owner_id = owners.first()
                .and_then(|o| o["id"].as_str())
                .ok_or_else(|| anyhow::anyhow!("Owner not found for namespace: {}", namespace))?;
            
            // Create a new git_allowed_repository
            let repo_id = create_git_allowed_repository(&api_base, namespace, &manifest.name, &manifest.version, owner_id, &access_token, &manifest.repository).await?;
            Some(repo_id)
        }
    };

    // Check if an action already exists for this name and namespace
    let action_exists = check_action_exists(&api_base, &manifest.name, namespace, &access_token).await?;
    let action_id = if action_exists {
        // Get the existing action ID
        get_action_id(&api_base, &manifest.name, namespace, &access_token).await?
    } else {
        // Create a new action with the git_allowed_repository_id
        create_action(&api_base, &manifest.name, &manifest.description, namespace, &profile_id, git_allowed_repository_id.as_deref(), manifest.kind.as_ref(), &access_token).await?
    };
    
    // Check if a version already exists for this action and version number
    let version_exists = check_version_exists(&api_base, &action_id, &manifest.version, &access_token).await?;
    
    if version_exists {
        anyhow::bail!(
            "Action version already exists: {}@{} in namespace '{}'. Use a different version number.",
            manifest.name, manifest.version, namespace
        );
    }
    
    // Create a new version
    let version_id = create_action_version(&api_base, &action_id, &manifest.version, &access_token).await?;
    
    // Update the action with the latest version ID
    update_action_latest_version(&api_base, &action_id, &version_id, &access_token).await?;
    
    // Create custom data types from the manifest
    create_custom_data_types(&api_base, &version_id, &manifest.types, &access_token).await?;
    
    // Insert action ports from the manifest
    insert_action_ports(&api_base, &version_id, &manifest.inputs, &manifest.outputs, &access_token).await?;
    
    println!("✅ Database updated successfully:");
    println!("   🏷️  Action: {} (ID: {})", manifest.name, action_id);
    println!("   📦 Version: {} (ID: {})", manifest.version, version_id);
    println!("   🔌 Ports: {} inputs, {} outputs", manifest.inputs.len(), manifest.outputs.len());
    println!("   📋 Custom types: {} types created", manifest.types.len());
    
    Ok(())
}

/// Checks if an action already exists for the given name and namespace
async fn check_action_exists(api_base: &str, action_name: &str, namespace: &str, access_token: &str) -> anyhow::Result<bool> {
    let client = reqwest::Client::new();
    
    println!("Checking if action exists: action_name = '{}', namespace = '{}'", action_name, namespace);
    
    // First get the owner ID for this namespace
    let owner_response = client
        .get(&format!("{}/rest/v1/owners?select=id&namespace=eq.{}", api_base, namespace))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    
    if !owner_response.status().is_success() {
        anyhow::bail!("Failed to get owner ID: {}", owner_response.status())
    }
    
    let owners: Vec<serde_json::Value> = owner_response.json().await?;
    let owner_id = owners.first()
        .and_then(|o| o["id"].as_str())
        .ok_or_else(|| anyhow::anyhow!("Owner not found for namespace: {}", namespace))?;
    
    println!("Found owner ID: {} for namespace: {}", owner_id, namespace);
    
    // Now check if action exists for this owner
    let response = client
        .get(&format!("{}/rest/v1/actions?select=id&name=eq.{}&rls_owner_id=eq.{}", 
            api_base, action_name, owner_id))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    
    if response.status().is_success() {
        let actions: Vec<serde_json::Value> = response.json().await?;
        Ok(!actions.is_empty())
    } else {
        anyhow::bail!("Failed to check action existence: {}", response.status())
    }
}

/// Gets the ID of an existing action
async fn get_action_id(api_base: &str, action_name: &str, namespace: &str, access_token: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    
    // First get the owner ID for this namespace
    let owner_response = client
        .get(&format!("{}/rest/v1/owners?select=id&namespace=eq.{}", api_base, namespace))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    
    if !owner_response.status().is_success() {
        anyhow::bail!("Failed to get owner ID: {}", owner_response.status())
    }
    
    let owners: Vec<serde_json::Value> = owner_response.json().await?;
    let owner_id = owners.first()
        .and_then(|o| o["id"].as_str())
        .ok_or_else(|| anyhow::anyhow!("Owner not found for namespace: {}", namespace))?;
    
    // Now get the action ID
    let response = client
        .get(&format!("{}/rest/v1/actions?select=id&name=eq.{}&rls_owner_id=eq.{}", 
            api_base, action_name, owner_id))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    
    if response.status().is_success() {
        let actions: Vec<serde_json::Value> = response.json().await?;
        if let Some(action) = actions.first() {
            Ok(action["id"].as_str().unwrap_or_default().to_string())
        } else {
            anyhow::bail!("Action not found")
        }
    } else {
        anyhow::bail!("Failed to get action ID: {}", response.status())
    }
}

/// Creates a new action
async fn create_action(api_base: &str, action_name: &str, description: &str, _namespace: &str, profile_id: &str, git_allowed_repository_id: Option<&str>, kind: Option<&crate::models::ShKind>, access_token: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    
    // First get the owner ID for this profile
    println!("Looking up owner for profile_id: {}", profile_id);
    let owner_response = client
        .get(&format!("{}/rest/v1/owners?select=id,profile_id,owner_type&profile_id=eq.{}&owner_type=eq.PROFILE", 
            api_base, profile_id))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    
    if !owner_response.status().is_success() {
        anyhow::bail!("Failed to get owner ID: {}", owner_response.status())
    }
    
    let owners: Vec<serde_json::Value> = owner_response.json().await?;
    println!("Found owners: {}", serde_json::to_string_pretty(&owners).unwrap_or_default());
    
    let owner_id = owners.first()
        .and_then(|o| o["id"].as_str())
        .ok_or_else(|| anyhow::anyhow!("Owner not found for profile"))?;
    
    println!("Using owner_id: {}", owner_id);
    
    // Convert ShKind to database enum value
    let kind_value = match kind {
        Some(crate::models::ShKind::Docker) => "DOCKER",
        Some(crate::models::ShKind::Wasm) => "WASM", 
        Some(crate::models::ShKind::Composition) => "COMPOSITION",
        None => "COMPOSITION", // Default to COMPOSITION if not specified
    };
    
    // Create the action
    let action_data = serde_json::json!({
        "name": action_name,
        "description": description,
        "rls_owner_id": owner_id,
        "git_allowed_repository_id": git_allowed_repository_id,
        "kind": kind_value
    });
    
    println!("Creating action with data: {}", serde_json::to_string_pretty(&action_data).unwrap_or_default());
    
    // Debug: Test can_act_on_owner function directly
    let debug_response = client
        .post(&format!("{}/rest/v1/rpc/can_act_on_owner", api_base))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({"p_owner_id": owner_id}))
        .send()
        .await?;
    
    if debug_response.status().is_success() {
        let can_act: serde_json::Value = debug_response.json().await?;
        println!("can_act_on_owner({}) returns: {}", owner_id, can_act);
    } else {
        println!("Failed to test can_act_on_owner: {}", debug_response.status());
    }
    
    let response = client
        .post(&format!("{}/rest/v1/actions", api_base))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .header("Prefer", "return=representation")
        .json(&action_data)
        .send()
        .await?;
    
    let status = response.status();
    if status.is_success() {
        let actions: Vec<serde_json::Value> = response.json().await?;
        if let Some(action) = actions.first() {
            Ok(action["id"].as_str().unwrap_or_default().to_string())
        } else {
            anyhow::bail!("Failed to get created action ID")
        }
    } else {
        let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
        anyhow::bail!("Failed to create action: {} - {}", status, error_text)
    }
}

/// Checks if a version already exists for the given action and version number
async fn check_version_exists(api_base: &str, action_id: &str, version_number: &str, access_token: &str) -> anyhow::Result<bool> {
    let client = reqwest::Client::new();
    
    let response = client
        .get(&format!("{}/rest/v1/action_versions?select=id&action_id=eq.{}&version_number=eq.{}", 
            api_base, action_id, version_number))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    
    if response.status().is_success() {
        let versions: Vec<serde_json::Value> = response.json().await?;
        Ok(!versions.is_empty())
    } else {
        anyhow::bail!("Failed to check version existence: {}", response.status())
    }
}

/// Creates a new action version
async fn create_action_version(api_base: &str, action_id: &str, version_number: &str, access_token: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    
    let version_data = serde_json::json!({
        "action_id": action_id,
        "version_number": version_number
    });
    
    let response = client
        .post(&format!("{}/rest/v1/action_versions", api_base))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .header("Prefer", "return=representation")
        .json(&version_data)
        .send()
        .await?;
    
    if response.status().is_success() {
        let versions: Vec<serde_json::Value> = response.json().await?;
        if let Some(version) = versions.first() {
            Ok(version["id"].as_str().unwrap_or_default().to_string())
        } else {
            anyhow::bail!("Failed to get created version ID")
        }
    } else {
        anyhow::bail!("Failed to create action version: {}", response.status())
    }
}

/// Looks up data_type_id from the data_types table for a given type name
async fn lookup_data_type_id(api_base: &str, type_name: &str, access_token: &str) -> anyhow::Result<Option<String>> {
    let client = reqwest::Client::new();
    
    // First try to find as a primitive type (exact match)
    let response = client
        .get(&format!("{}/rest/v1/data_types?select=id&name=eq.{}&is_primitive=eq.true", 
            api_base, type_name))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    
    if response.status().is_success() {
        let data_types: Vec<serde_json::Value> = response.json().await?;
        if let Some(data_type) = data_types.first() {
            if let Some(id) = data_type["id"].as_str() {
                return Ok(Some(id.to_string()));
            }
        }
    }
    
    // If not found as primitive, try case-insensitive search for primitive types
    let response = client
        .get(&format!("{}/rest/v1/data_types?select=id&name=ilike.{}&is_primitive=eq.true", 
            api_base, type_name))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    
    if response.status().is_success() {
        let data_types: Vec<serde_json::Value> = response.json().await?;
        if let Some(data_type) = data_types.first() {
            if let Some(id) = data_type["id"].as_str() {
                return Ok(Some(id.to_string()));
            }
        }
    }
    
    // If not found as primitive, try to find as a custom type (exact match)
    let response = client
        .get(&format!("{}/rest/v1/data_types?select=id&name=eq.{}&is_primitive=eq.false", 
            api_base, type_name))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    
    if response.status().is_success() {
        let data_types: Vec<serde_json::Value> = response.json().await?;
        if let Some(data_type) = data_types.first() {
            if let Some(id) = data_type["id"].as_str() {
                return Ok(Some(id.to_string()));
            }
        }
    }
    
    // If not found as custom type, try case-insensitive search for custom types
    let response = client
        .get(&format!("{}/rest/v1/data_types?select=id&name=ilike.{}&is_primitive=eq.false", 
            api_base, type_name))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    
    if response.status().is_success() {
        let data_types: Vec<serde_json::Value> = response.json().await?;
        if let Some(data_type) = data_types.first() {
            if let Some(id) = data_type["id"].as_str() {
                return Ok(Some(id.to_string()));
            }
        }
    }
    
    Ok(None)
}

/// Creates custom data types from the manifest types field
async fn create_custom_data_types(
    api_base: &str, 
    version_id: &str, 
    types: &std::collections::HashMap<String, serde_json::Value>, 
    access_token: &str
) -> anyhow::Result<()> {
    if types.is_empty() {
        return Ok(());
    }
    
    let client = reqwest::Client::new();
    
    // Get the owner ID for this version (needed for RLS)
    let version_response = client
        .get(&format!("{}/rest/v1/action_versions?select=rls_owner_id&id=eq.{}", 
            api_base, version_id))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    
    if !version_response.status().is_success() {
        anyhow::bail!("Failed to get version owner ID: {}", version_response.status())
    }
    
    let versions: Vec<serde_json::Value> = version_response.json().await?;
    let owner_id = versions.first()
        .and_then(|v| v["rls_owner_id"].as_str())
        .ok_or_else(|| anyhow::anyhow!("Version owner ID not found"))?;
    
    println!("Creating {} custom data types...", types.len());
    
    // Create each custom data type
    for (type_name, schema) in types {
        let data_type_data = serde_json::json!({
            "action_version_id": version_id,
            "name": type_name,
            "description": format!("TypeScript definition for {}", type_name),
            "schema": schema,
            "is_primitive": false,
            "rls_owner_id": owner_id
        });
        
        println!("Creating custom data type: {} with schema: {}", type_name, serde_json::to_string_pretty(schema).unwrap_or_default());
        
        let response = client
            .post(&format!("{}/rest/v1/data_types", api_base))
            .header("apikey", crate::config::SUPABASE_ANON_KEY)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .json(&data_type_data)
            .send()
            .await?;
        
        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            anyhow::bail!("Failed to create custom data type '{}': {} - {}", type_name, status, error_text)
        }
        
        println!("✅ Created custom data type: {}", type_name);
    }
    
    println!("✅ Successfully created {} custom data types", types.len());
    Ok(())
}

/// Inserts action ports for inputs and outputs
async fn insert_action_ports(api_base: &str, version_id: &str, inputs: &[ShPort], outputs: &[ShPort], access_token: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    
    // Get the owner ID for this version (needed for RLS)
    let version_response = client
        .get(&format!("{}/rest/v1/action_versions?select=rls_owner_id&id=eq.{}", 
            api_base, version_id))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    
    if !version_response.status().is_success() {
        anyhow::bail!("Failed to get version owner ID: {}", version_response.status())
    }
    
    let versions: Vec<serde_json::Value> = version_response.json().await?;
    let owner_id = versions.first()
        .and_then(|v| v["rls_owner_id"].as_str())
        .ok_or_else(|| anyhow::anyhow!("Version owner ID not found"))?;
    
    // Insert input ports
    for input in inputs {
        // Map ShType to data type name
        let type_name = match &input.ty {
            ShType::String => "string",
            ShType::Integer => "integer", 
            ShType::Boolean => "boolean",
            ShType::Object => "object",
            ShType::Array => "array",
            ShType::Number => "number",
            ShType::Custom(name) => name.as_str(),
        };
        
        // Look up the data_type_id
        let data_type_id = lookup_data_type_id(api_base, type_name, access_token).await?;
        
        let mut port_data = serde_json::json!({
            "name": input.name,
            "description": input.description,
            "action_port_direction": "INPUT",
            "action_version_id": version_id,
            "rls_owner_id": owner_id,
            "is_required": input.required,
            "default": input.default
        });
        
        // Add data_type_id if found
        if let Some(dt_id) = data_type_id {
            port_data["data_type_id"] = serde_json::Value::String(dt_id);
        }
        
        println!("Inserting input port with data: {}", serde_json::to_string_pretty(&port_data).unwrap_or_default());
        
        let response = client
            .post(&format!("{}/rest/v1/action_ports", api_base))
            .header("apikey", crate::config::SUPABASE_ANON_KEY)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .json(&port_data)
            .send()
            .await?;
        
        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            anyhow::bail!("Failed to insert input port: {} - {}", status, error_text)
        }
    }
    
    // Insert output ports
    for output in outputs {
        // Map ShType to data type name
        let type_name = match &output.ty {
            ShType::String => "string",
            ShType::Integer => "integer",
            ShType::Boolean => "boolean", 
            ShType::Object => "object",
            ShType::Array => "array",
            ShType::Number => "number",
            ShType::Custom(name) => name.as_str(),
        };
        
        // Look up the data_type_id
        let data_type_id = lookup_data_type_id(api_base, type_name, access_token).await?;
        
        let mut port_data = serde_json::json!({
            "name": output.name,
            "description": output.description,
            "action_port_direction": "OUTPUT",
            "action_version_id": version_id,
            "rls_owner_id": owner_id,
            "is_required": output.required,
            "default": output.default
        });
        
        // Add data_type_id if found
        if let Some(dt_id) = data_type_id {
            port_data["data_type_id"] = serde_json::Value::String(dt_id);
        }
        
        println!("Inserting output port with data: {}", serde_json::to_string_pretty(&port_data).unwrap_or_default());
        
        let response = client
            .post(&format!("{}/rest/v1/action_ports", api_base))
            .header("apikey", crate::config::SUPABASE_ANON_KEY)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .json(&port_data)
            .send()
            .await?;
        
        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            anyhow::bail!("Failed to insert output port: {} - {}", status, error_text)
        }
    }
    
    Ok(())
}

/// Checks if a git_allowed_repository exists for the given namespace, name, and version
async fn check_git_allowed_repository_exists(api_base: &str, namespace: &str, name: &str, version: &str, access_token: &str) -> anyhow::Result<Option<String>> {
    let client = reqwest::Client::new();
    
    println!("Checking if git_allowed_repository exists: namespace = '{}', name = '{}', version = '{}'", namespace, name, version);
    
    // First get the owner ID for this namespace
    let owner_response = client
        .get(&format!("{}/rest/v1/owners?select=id&namespace=eq.{}", api_base, namespace))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    
    if !owner_response.status().is_success() {
        anyhow::bail!("Failed to get owner ID: {}", owner_response.status())
    }
    
    let owners: Vec<serde_json::Value> = owner_response.json().await?;
    let owner_id = owners.first()
        .and_then(|o| o["id"].as_str())
        .ok_or_else(|| anyhow::anyhow!("Owner not found for namespace: {}", namespace))?;
    
    println!("Found owner ID: {} for namespace: {}", owner_id, namespace);
    
    // Check if git_allowed_repository exists for this owner, name, and version
    // We'll use the name as the slug and create a full_name from namespace/name
    let full_name = format!("{}/{}", namespace, name);
    let response = client
        .get(&format!("{}/rest/v1/git_allowed_repositories?select=id&owner_id=eq.{}&full_name=eq.{}", 
            api_base, owner_id, full_name))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    
    if response.status().is_success() {
        let repos: Vec<serde_json::Value> = response.json().await?;
        if let Some(repo) = repos.first() {
            let repo_id = repo["id"].as_str().unwrap_or_default().to_string();
            println!("Found existing git_allowed_repository: {}", repo_id);
            Ok(Some(repo_id))
        } else {
            println!("No existing git_allowed_repository found");
            Ok(None)
        }
    } else {
        anyhow::bail!("Failed to check git_allowed_repository existence: {}", response.status())
    }
}

/// Creates a new git_allowed_repository for the given namespace, name, and version
async fn create_git_allowed_repository(api_base: &str, namespace: &str, name: &str, version: &str, owner_id: &str, access_token: &str, repository_url: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    
    println!("Creating new git_allowed_repository: namespace = '{}', name = '{}', version = '{}'", namespace, name, version);
    
    // Create the git_allowed_repository data
    let full_name = format!("{}/{}", namespace, name);
    let repo_data = serde_json::json!({
        "namespace": namespace,
        "slug": name,
        "full_name": full_name,
        "owner_id": owner_id,
        "git_provider": "GITHUB",
        "external_id": null,
        "git_app_installation_id": null,
        "url": repository_url
    });
    
    println!("Creating git_allowed_repository with data: {}", serde_json::to_string_pretty(&repo_data).unwrap_or_default());
    
    let response = client
        .post(&format!("{}/rest/v1/git_allowed_repositories", api_base))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .header("Prefer", "return=representation")
        .json(&repo_data)
        .send()
        .await?;
    
    let status = response.status();
    if status.is_success() {
        let repos: Vec<serde_json::Value> = response.json().await?;
        if let Some(repo) = repos.first() {
            let repo_id = repo["id"].as_str().unwrap_or_default().to_string();
            println!("✅ Created git_allowed_repository: {}", repo_id);
            Ok(repo_id)
        } else {
            anyhow::bail!("Failed to get created git_allowed_repository ID")
        }
    } else {
        let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
        anyhow::bail!("Failed to create git_allowed_repository: {} - {}", status, error_text)
    }
}

/// Updates the actions table to set the latest_action_version_id
async fn update_action_latest_version(api_base: &str, action_id: &str, version_id: &str, access_token: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    
    println!("Updating action {} with latest_action_version_id: {}", action_id, version_id);
    
    let update_data = serde_json::json!({
        "latest_action_version_id": version_id
    });
    
    let response = client
        .patch(&format!("{}/rest/v1/actions?id=eq.{}", api_base, action_id))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&update_data)
        .send()
        .await?;
    
    let status = response.status();
    if status.is_success() {
        println!("✅ Successfully updated action with latest_action_version_id");
        Ok(())
    } else {
        let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
        anyhow::bail!("Failed to update action latest version: {} - {}", status, error_text)
    }
}


