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
) -> Result<String> {
    if which::which("wasmtime").is_err() {
        log_error("wasmtime not found in PATH", Some(&action.id));
        bail!("wasmtime not found in PATH");
    }

    
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

    println!("Input: {}", input_json);
    println!("Module path: {:?}", module_path);
    println!("Action ID: {}", action.id);
    println!("--------------------------------");
    log_info(&format!("Running WASM file: {:?}", module_path), Some(&action.id));
    log_info(&format!("Input: {}", input_json), Some(&action.id));
    
    // Construct command
    let mut cmd = TokioCommand::new("wasmtime");
    
    // Add permissions based on the action's permissions
    if let Some(permissions) = &action.permissions {
        // Add filesystem permissions only if the fs vector is not empty
        if !permissions.fs.is_empty() {
            let mut has_cli = false;
            for fs_perm in &permissions.fs {
                match fs_perm.as_str() {
                    "read" | "write" => {
                        if !has_cli {
                            cmd.arg("-S").arg("cli");
                            has_cli = true; // Only add -S cli once
                        }
                    },
                    _ => {
                        log_info(&format!("Unknown filesystem permission: {}", fs_perm), Some(&action.id));
                    }
                }
            }
        }
        
        // Add network permissions only if the net vector is not empty
        if !permissions.net.is_empty() {
            let mut has_http = false;
            for net_perm in &permissions.net {
                match net_perm.as_str() {
                    "http" | "https" => {
                        if !has_http {
                            cmd.arg("-S").arg("http");
                            has_http = true; // Only add -S http once (wasmtime uses 'http' for both http and https)
                        }
                    },
                    _ => {
                        log_info(&format!("Unknown network permission: {}", net_perm), Some(&action.id));
                    }
                }
            }
        }
    } else {
        // Default permissions if none specified
        log_info("No permissions specified, using default", Some(&action.id));
    }
    
    // Mount the working directory for filesystem access
    // Check if the action has filesystem permissions (read or write)
    println!("Permissions: {:#?}", action.permissions);
    if let Some(permissions) = &action.permissions {
        if !permissions.fs.is_empty() {
            // If filesystem permissions are required, mount the directory based on the first input (file path)
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
                            
                            log_info(&format!("Mounting directory for filesystem access (permissions: {:?}): {}", permissions.fs, dir_to_mount), Some(&action.id));
                        }
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
        let mut output = String::new();
        let mut line = String::new();
        while out_reader.read_line(&mut line).await.unwrap_or(0) > 0 {
            output.push_str(&line);
            line.clear();
        }
        
        // Send the raw output as a string
        let _ = tx.send(Value::String(output.trim().to_string()));
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

    // Collect the result from the WASM module
    let mut results = Vec::new();
    while let Ok(v) = rx.try_recv() { 
        results.push(v);
    }
    
    // Return the first result as a string
    if results.is_empty() {
        Ok(String::new())
    } else {
        let first_result = &results[0];
        if let Some(string_val) = first_result.as_str() {
            Ok(string_val.to_string())
        } else {
            Ok(first_result.to_string())
        }
    }
}

/// Downloads a WASM module from the registry or mirrors
pub async fn download_wasm(
    action_ref: &str, 
    mirrors: &[String], 
    cache_dir: &PathBuf
) -> Result<PathBuf> {
    // Construct the WASM file path in the cache directory with proper directory structure
    let url_path = action_ref.replace(":", "/");
    let wasm_dir = cache_dir.join(&url_path);
    let wasm_path = wasm_dir.join("artifact.wasm");
    
    // Create the directory if it doesn't exist
    std::fs::create_dir_all(&wasm_dir)?;
    
    // Check if the WASM file already exists
    if wasm_path.exists() {
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
