use crate::models::ShAction;
use anyhow::{bail, Result};
use serde_json::Value;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc;
use zip::ZipArchive;
use std::fs::File;
use std::io::Read;

/// Executes a WASM step by downloading and running the WASM module
pub async fn run_wasm_step(
    action: &ShAction,
    inputs: &Value,
    cache_dir: &PathBuf,
    log_info: &(dyn Fn(&str, Option<&str>) + Send + Sync),
    log_success: &(dyn Fn(&str, Option<&str>) + Send + Sync),
    log_error: &(dyn Fn(&str, Option<&str>) + Send + Sync),
) -> Result<Vec<Value>> {
    if which::which("wasmtime").is_err() {
        log_error("wasmtime not found in PATH", Some(&action.id));
        bail!("wasmtime not found in PATH");
    }

    log_info(&format!("Downloading WASM module: {}", action.uses), Some(&action.id));
    // For now, we'll create a simple implementation that downloads the WASM file
    // In a real implementation, this would download from the registry
    let module_path = download_wasm(&action.uses, &action.mirrors, cache_dir).await?;
    log_success(&format!("WASM module downloaded: {:?}", module_path), Some(&action.id));
    
    // Verify the WASM file exists and is readable
    if !module_path.exists() {
        return Err(anyhow::anyhow!("WASM file not found at: {:?}", module_path));
    }
    
    // Check if the file is readable
    if let Err(e) = std::fs::metadata(&module_path) {
        return Err(anyhow::anyhow!("WASM file not accessible at {:?}: {}", module_path, e));
    }

    // build stdin payload - use the pre-built parameters
    let input_json = serde_json::to_string(inputs)?;

    log_info(&format!("Running WASM file: {:?}", module_path), Some(&action.id));
    log_info(&format!("Input: {}", input_json), Some(&action.id));
    
    // Construct command
    let mut cmd = TokioCommand::new("wasmtime");
    
    // Add permissions based on the action's permissions
    if let Some(permissions) = &action.permissions {
        // Add filesystem permissions
        for fs_perm in &permissions.fs {
            match fs_perm.as_str() {
                "read" => {
                    cmd.arg("-S").arg("cli");
                },
                "write" => {
                    cmd.arg("-S").arg("cli");
                },
                _ => {
                    log_info(&format!("Unknown filesystem permission: {}", fs_perm), Some(&action.id));
                }
            }
        }
        
        // Add network permissions
        for net_perm in &permissions.net {
            match net_perm.as_str() {
                "http" => {
                    cmd.arg("-S").arg("http");
                },
                "https" => {
                    cmd.arg("-S").arg("http"); // wasmtime uses 'http' for both http and https
                },
                _ => {
                    log_info(&format!("Unknown network permission: {}", net_perm), Some(&action.id));
                }
            }
        }
    } else {
        // Default permissions if none specified
        log_info("No permissions specified, using default", Some(&action.id));
    }
    
    // Mount the working directory for filesystem access
    // Special handling for std/read-file and std/write-file actions
    if action.uses.starts_with("std/read-file:") || action.uses.starts_with("std/write-file:") {
        // For read-file and write-file, use the first input as the file path and mount its parent directory
        if let Some(inputs_array) = inputs.as_array() {
            if let Some(first_input) = inputs_array.first() {
                if let Some(file_path) = first_input.as_str() {
                    if file_path.starts_with('/') {
                        let path = std::path::Path::new(file_path);
                        let dir_to_mount = if path.is_file() {
                            // If it's a file, use its parent directory
                            path.parent()
                                .and_then(|p| p.to_str())
                                .unwrap_or(file_path)
                        } else {
                            // If it's a directory or doesn't exist, use as-is
                            file_path
                        };
                        
                        cmd.arg("--dir").arg(dir_to_mount);
                        cmd.current_dir(dir_to_mount);
                        
                        log_info(&format!("Mounting directory for read-file: {}", dir_to_mount), Some(&action.id));
                    }
                }
            }
        }
    }
    
    cmd.arg(&module_path);

    // spawn with piped stdio
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn wasmtime for step {}: {}", action.id, e))?;

    // feed stdin JSON
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(input_json.as_bytes()).await?;
        // Let's send an array of fixed inputs [2.5, "ignored_value"]
        // stdin.write_all(b"[\"2.5\", \"ignored_value\"]").await?;
    }
    drop(child.stdin.take());

    // pump stdout/stderr and collect patches
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut out_reader = BufReader::new(stdout);
    let mut err_reader = BufReader::new(stderr);

    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();

    let pump_out = tokio::spawn(async move {
        let mut line = String::new();
        while out_reader.read_line(&mut line).await.unwrap_or(0) > 0 {
            // Try to parse the line directly as JSON
            if let Ok(v) = serde_json::from_str::<Value>(line.trim()) {
                let _ = tx.send(v);
            }
            line.clear();
        }
    });

    let pump_err = tokio::spawn(async move {
        let mut line = String::new();
        while err_reader.read_line(&mut line).await.unwrap_or(0) > 0 {
            // Just consume stderr for now
            line.clear();
        }
    });

    let status = child.wait().await?;
    let _ = pump_out.await;
    let _ = pump_err.await;

    if !status.success() {
        log_error(&format!("WASM execution failed with status: {}", status), Some(&action.id));
        bail!("step '{}' failed with {}", action.id, status);
    }
    
    log_success("WASM execution completed successfully", Some(&action.id));

    // Collect the first result from the WASM module
    let mut results = Vec::new();
    while let Ok(v) = rx.try_recv() { 
        results.push(v);
    }
    
    // The WASM module outputs a single JSON array, so we take the first result
    if results.is_empty() {
        // If no results, return an empty vector
        Ok(Vec::new())
    } else {
        // Take the first result and parse it as an array
        let first_result = &results[0];
        if let Some(array) = first_result.as_array() {
            Ok(array.clone())
        } else {
            // If it's not an array, wrap it in a single-element array
            Ok(vec![first_result.clone()])
        }
    }
}

/// Downloads a WASM module from the registry or mirrors
pub async fn download_wasm(
    action_ref: &str, 
    mirrors: &[String], 
    cache_dir: &PathBuf
) -> Result<PathBuf> {
    println!("Downloading WASM file for action: {}", action_ref);
    // Construct the WASM file path in the cache directory with proper directory structure
    let url_path = action_ref.replace(":", "/");
    let wasm_dir = cache_dir.join(&url_path);
    let wasm_path = wasm_dir.join("artifact.wasm");
    
    // Create the directory if it doesn't exist
    std::fs::create_dir_all(&wasm_dir)?;
    
    // Check if the WASM file already exists
    if wasm_path.exists() {
        println!("WASM file already exists: {:?}", wasm_path);
        return Ok(wasm_path);
    }
    
    let client = reqwest::Client::new();
    
    // First try the default registry
    let parts: Vec<&str> = action_ref.split(':').collect();
    if parts.len() != 2 {
        return Err(anyhow::anyhow!("Invalid action reference format: {}", action_ref));
    }
    
    let (namespace_slug, version) = (parts[0], parts[1]);
    let namespace_parts: Vec<&str> = namespace_slug.split('/').collect();
    if namespace_parts.len() != 2 {
        return Err(anyhow::anyhow!("Invalid namespace format: {}", namespace_slug));
    }
    
    let (namespace, slug) = (namespace_parts[0], namespace_parts[1]);
    let default_url = format!("https://api.starthub.so/storage/v1/object/public/artifacts/{}/{}/{}/artifact.zip", namespace, slug, version);
    println!("Trying to download from default registry: {}", default_url);
    
    match try_download_from_url(&client, &default_url, &wasm_dir, &wasm_path).await {
        Ok(path) => {
            println!("Successfully downloaded from default registry");
            return Ok(path);
        },
        Err(e) => {
            println!("Failed to download from default registry: {}", e);
        }
    }
    
    // If default registry failed, try mirrors
    for mirror in mirrors {
        // Use the mirror URL as-is since it already contains the full path
        let url = mirror.clone();
        println!("Trying to download from mirror: {}", url);
        
        match try_download_from_url(&client, &url, &wasm_dir, &wasm_path).await {
            Ok(path) => {
                println!("Successfully downloaded from mirror: {}", url);
                return Ok(path);
            },
            Err(e) => {
                println!("Failed to download from mirror {}: {}", url, e);
                continue;
            }
        }
    }
    
    Err(anyhow::anyhow!("Failed to download WASM file from all sources"))
}

/// Tries to download a WASM file from a specific URL
async fn try_download_from_url(
    client: &reqwest::Client,
    url: &str,
    wasm_dir: &std::path::Path,
    wasm_path: &std::path::Path,
) -> Result<PathBuf> {
    let response = client.get(url).send().await?;
    
    if response.status().is_success() {
        let zip_bytes = response.bytes().await?;
        
        // Create a temporary file for the zip
        let temp_zip_path = wasm_dir.join("temp_artifact.zip");
        std::fs::write(&temp_zip_path, zip_bytes)?;
        
        // Extract the WASM file from the zip
        extract_wasm_from_zip(&temp_zip_path, wasm_path).await?;
        
        // Clean up the temporary zip file
        std::fs::remove_file(&temp_zip_path)?;
        
        println!("WASM file extracted to: {:?}", wasm_path);
        Ok(wasm_path.to_path_buf())
    } else {
        Err(anyhow::anyhow!("HTTP error: {}", response.status()))
    }
}

/// Extracts a WASM file from a ZIP archive
async fn extract_wasm_from_zip(
    zip_path: &std::path::Path, 
    wasm_path: &std::path::Path
) -> Result<()> {
    let file = File::open(zip_path)?;
    let mut archive = ZipArchive::new(file)?;
    
    // Find the WASM file in the archive
    for i in 0..archive.len() {
        let file = archive.by_index(i)?;
        if file.name().ends_with(".wasm") {
            let mut wasm_content = Vec::new();
            let mut reader = std::io::BufReader::new(file);
            reader.read_to_end(&mut wasm_content)?;
            
            // Write the WASM content to the target path
            std::fs::write(wasm_path, wasm_content)?;
            return Ok(());
        }
    }
    
    Err(anyhow::anyhow!("No WASM file found in ZIP archive"))
}
