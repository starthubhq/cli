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

/// Executes a Docker step by running the referenced container image
/// The container is expected to read JSON from stdin and print a JSON array on stdout
pub async fn run_docker_step(
    action: &ShAction,
    inputs: &Value,
    _cache_dir: &PathBuf,
    log_info: &(dyn Fn(&str, Option<&str>) + Send + Sync),
    log_success: &(dyn Fn(&str, Option<&str>) + Send + Sync),
    log_error: &(dyn Fn(&str, Option<&str>) + Send + Sync),
) -> Result<String> {
    // Ensure docker is available
    if which::which("docker").is_err() {
        log_error("docker not found in PATH", Some(&action.id));
        bail!("docker not found in PATH");
    }

    // Build stdin payload from provided inputs
    let input_json = serde_json::to_string(inputs)?;

    // Download the Docker image artifact from registry/mirrors
    let image_path = download_docker(&action.uses, &action.mirrors, _cache_dir).await?;
    log_success(&format!("Docker image downloaded: {:?}", image_path), Some(&action.id));
    
    // Verify the Docker image exists and is readable
    if !image_path.exists() {
        return Err(anyhow::anyhow!("Docker image not found at: {:?}", image_path));
    }
    
    // Check if the file is readable
    if let Err(e) = std::fs::metadata(&image_path) {
        return Err(anyhow::anyhow!("Docker image not accessible at {:?}: {}", image_path, e));
    }

    // The `uses` field should contain the docker image reference (e.g., "starthub-ssh" or "org/image:tag")
    let image_ref = &action.uses;
    if image_ref.trim().is_empty() {
        bail!("docker image reference (uses) is empty for step {}", action.id);
    }

    log_info(&format!("Running Docker image: {}", image_ref), Some(&action.id));
    log_info(&format!("Input: {}", input_json), Some(&action.id));

    // Load the Docker image from the downloaded tar file
    let load_result = TokioCommand::new("docker")
        .arg("load")
        .arg("-i")
        .arg(&image_path)
        .output()
        .await?;
    
    if !load_result.status.success() {
        log_error(&format!("Failed to load Docker image: {}", String::from_utf8_lossy(&load_result.stderr)), Some(&action.id));
        bail!("Failed to load Docker image from {:?}", image_path);
    }
    
    // Extract the image name from the load output
    let load_output = String::from_utf8_lossy(&load_result.stdout);
    let image_name = if let Some(line) = load_output.lines().find(|line| line.contains("Loaded image:")) {
        line.split("Loaded image: ").nth(1).unwrap_or(image_ref).trim()
    } else {
        image_ref
    };
    
    log_info(&format!("Loaded Docker image: {}", image_name), Some(&action.id));
    
    // Construct docker run command: docker run -i --rm <image>
    let mut cmd = TokioCommand::new("docker");
    cmd.arg("run").arg("-i").arg("--rm").arg(image_name);

    // Spawn with piped stdio
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn docker for step {}: {}", action.id, e))?;

    // feed stdin JSON
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(input_json.as_bytes()).await?;
    }
    drop(child.stdin.take());

    // pump stdout/stderr and collect JSON outputs
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
        // Send the entire output as a single string
        let _ = tx.send(Value::String(output.trim().to_string()));
    });

    let pump_err = tokio::spawn(async move {
        let mut line = String::new();
        while err_reader.read_line(&mut line).await.unwrap_or(0) > 0 {
            // Consume stderr for now; logs may be verbose
            line.clear();
        }
    });

    let status = child.wait().await?;
    let _ = pump_out.await;
    let _ = pump_err.await;

    if !status.success() {
        log_error(&format!("Docker execution failed with status: {}", status), Some(&action.id));
        bail!("step '{}' failed with {}", action.id, status);
    }

    log_success("Docker execution completed successfully", Some(&action.id));

    // Collect the result from the container
    let mut results = Vec::new();
    while let Ok(v) = rx.try_recv() {
        results.push(v);
    }

    println!("results: {:#?}", results);
    
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

/// Downloads a Docker image from the registry or mirrors
pub async fn download_docker(
    action_ref: &str, 
    mirrors: &[String], 
    cache_dir: &PathBuf
) -> Result<PathBuf> {
    // Construct the Docker image path in the cache directory with proper directory structure
    let url_path = action_ref.replace(":", "/");
    let docker_dir = cache_dir.join(&url_path);
    let docker_path = docker_dir.join("artifact.tar");
    
    // Create the directory if it doesn't exist
    std::fs::create_dir_all(&docker_dir)?;
    
    // Check if the Docker image already exists
    if docker_path.exists() {
        return Ok(docker_path);
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
    
    match try_download_from_url(&client, &default_url, &docker_dir, &docker_path).await {
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
        
        match try_download_from_url(&client, &url, &docker_dir, &docker_path).await {
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
    
    Err(anyhow::anyhow!("Failed to download Docker image from all sources"))
}

/// Tries to download a Docker image from a specific URL
async fn try_download_from_url(
    client: &reqwest::Client,
    url: &str,
    docker_dir: &std::path::Path,
    docker_path: &std::path::Path,
) -> Result<PathBuf> {
    let response = client.get(url).send().await?;
    
    if response.status().is_success() {
        let zip_bytes = response.bytes().await?;
        
        // Create a temporary file for the zip
        let temp_zip_path = docker_dir.join("temp_artifact.zip");
        std::fs::write(&temp_zip_path, zip_bytes)?;
        
        // Extract the Docker image from the zip
        extract_docker_from_zip(&temp_zip_path, docker_path).await?;
        
        // Clean up the temporary zip file
        std::fs::remove_file(&temp_zip_path)?;
        
        println!("Docker image extracted to: {:?}", docker_path);
        Ok(docker_path.to_path_buf())
    } else {
        Err(anyhow::anyhow!("HTTP error: {}", response.status()))
    }
}

/// Extracts a Docker image from a ZIP archive
async fn extract_docker_from_zip(
    zip_path: &std::path::Path, 
    docker_path: &std::path::Path
) -> Result<()> {
    let file = File::open(zip_path)?;
    let mut archive = ZipArchive::new(file)?;
    
    // Find the Docker image file in the archive
    for i in 0..archive.len() {
        let file = archive.by_index(i)?;
        if file.name().ends_with(".tar") || file.name().ends_with(".tar.gz") {
            let mut docker_content = Vec::new();
            let mut reader = std::io::BufReader::new(file);
            reader.read_to_end(&mut docker_content)?;
            
            // Write the Docker image content to the target path
            std::fs::write(docker_path, docker_content)?;
            return Ok(());
        }
    }
    
    Err(anyhow::anyhow!("No Docker image file found in ZIP archive"))
}


