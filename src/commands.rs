use anyhow::Result;
use std::{fs, path::Path, io::Write};
use std::process::Command as PCommand;
use std::process::Stdio;
use inquire::{Text, Select};
use tokio::time::{sleep, Duration};
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader};
use webbrowser;
use reqwest;

use crate::models::{ShManifest, ShKind, ShPort, ShType};
use crate::templates;

// Global constants for local development server
const LOCAL_SERVER_URL: &str = "http://127.0.0.1:3000";
const LOCAL_SERVER_HOST: &str = "127.0.0.1:3000";


pub async fn cmd_publish_docker_inner(m: &ShManifest, no_build: bool) -> anyhow::Result<()> {
    // Implementation for Docker publishing
    println!("🐳 Publishing Docker image for {}", m.name);

    if !no_build {
        // Build Docker image
        let dockerfile_path = Path::new("Dockerfile");
        if !dockerfile_path.exists() {
            return Err(anyhow::anyhow!("Dockerfile not found in current directory"));
        }
        
        let build_cmd = PCommand::new("docker")
            .args(&["build", "-t", &format!("{}:{}", m.name, m.version), "."])
            .output()?;
            
        if !build_cmd.status.success() {
            return Err(anyhow::anyhow!("Docker build failed: {}", String::from_utf8_lossy(&build_cmd.stderr)));
        }
        
        println!("✅ Docker image built successfully");
    }
    
    // Tag and push to registry
    let image_name = format!("{}:{}", m.name, m.version);
    let registry_image = format!("registry.starthub.so/{}:{}", m.name, m.version);
    
    // Tag for registry
    let tag_cmd = PCommand::new("docker")
        .args(&["tag", &image_name, &registry_image])
        .output()?;
        
    if !tag_cmd.status.success() {
        return Err(anyhow::anyhow!("Docker tag failed: {}", String::from_utf8_lossy(&tag_cmd.stderr)));
    }
    
    // Push to registry
    let push_cmd = PCommand::new("docker")
        .args(&["push", &registry_image])
        .output()?;
        
    if !push_cmd.status.success() {
        return Err(anyhow::anyhow!("Docker push failed: {}", String::from_utf8_lossy(&push_cmd.stderr)));
    }
    
    println!("✅ Docker image pushed to registry: {}", registry_image);
    Ok(())
}

pub async fn cmd_publish_wasm_inner(m: &ShManifest, no_build: bool) -> anyhow::Result<()> {
    // Implementation for WASM publishing
    println!("🦀 Publishing WASM module for {}", m.name);

    if !no_build {
        // Build WASM module
        let build_cmd = PCommand::new("cargo")
            .args(&["build", "--release", "--target", "wasm32-wasi"])
            .output()?;
            
        if !build_cmd.status.success() {
            return Err(anyhow::anyhow!("WASM build failed: {}", String::from_utf8_lossy(&build_cmd.stderr)));
        }
        
        println!("✅ WASM module built successfully");
    }
    
    // Package WASM module
    let wasm_path = format!("target/wasm32-wasi/release/{}.wasm", m.name);
    if !Path::new(&wasm_path).exists() {
        return Err(anyhow::anyhow!("WASM file not found: {}", wasm_path));
    }
    
    // Create zip package
    let zip_path = format!("{}.zip", m.name);
    let zip_file = fs::File::create(&zip_path)?;
    let mut zip = zip::ZipWriter::new(zip_file);
    
    let options = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o755);
    
    zip.start_file(&format!("{}.wasm", m.name), options)?;
    let wasm_data = fs::read(&wasm_path)?;
    zip.write_all(&wasm_data)?;
    zip.finish()?;
    
    println!("✅ WASM module packaged: {}", zip_path);
    Ok(())
}

pub async fn cmd_init(path: String) -> anyhow::Result<()> {
    // Basic fields
    let name = Text::new("Package name:")
        .with_default("http-get-wasm")
        .prompt()?;

    let version = Text::new("Version:")
        .with_default("0.1.0")
        .prompt()?;

    let kind_options = vec![
        ("Wasm", ShKind::Wasm),
        ("Docker", ShKind::Docker),
        ("Composition", ShKind::Composition),
    ];
    let kind_choice = Select::new("Package type:", kind_options.iter().map(|(name, _)| *name).collect())
        .prompt()?;
    let kind = kind_options.iter().find(|(name, _)| *name == kind_choice).unwrap().1.clone();

    // Repository
    let repo_default = match kind {
        ShKind::Wasm   => "github.com/starthubhq/http-get-wasm",
        ShKind::Docker => "github.com/starthubhq/http-get-wasm",
        ShKind::Composition => "github.com/starthubhq/composite-action",
    };
    let repository = Text::new("Repository:")
        .with_default(repo_default)
        .prompt()?;

    // Create manifest
    let manifest = ShManifest {
        name: name.clone(),
        version: version.clone(),
        kind: Some(kind.clone()),
        description: "A StartHub package".to_string(),
        flow_control: false,
        repository,
        manifest_version: 1,
        image: None,
        license: "MIT".to_string(),
        inputs: vec![
        ShPort {
                name: "input".to_string(),
                description: "Input parameter".to_string(),
                ty: ShType::String,
            required: true,
            default: None,
        }
        ],
        outputs: vec![
        ShPort {
                name: "output".to_string(),
                description: "Output result".to_string(),
                ty: ShType::String,
            required: true,
            default: None,
        }
        ],
        types: std::collections::HashMap::new(),
        steps: vec![],
        wires: vec![],
        export: serde_json::json!({}),
    };

    // Write starthub.json
    let starthub_path = Path::new(&path).join("starthub.json");
    let starthub_json = serde_json::to_string_pretty(&manifest)?;
    fs::write(&starthub_path, starthub_json)?;

    println!("✅ Created starthub.json in {}", starthub_path.display());

    // Create basic files based on type
    match kind {
        ShKind::Wasm => {
            // Create Cargo.toml for WASM
            let cargo_toml = templates::wasm_cargo_toml_tpl(&name, &version);
            let cargo_path = Path::new(&path).join("Cargo.toml");
            fs::write(&cargo_path, cargo_toml)?;
            
            // Create src/main.rs
            let src_dir = Path::new(&path).join("src");
            fs::create_dir_all(&src_dir)?;
            let main_rs = templates::WASM_MAIN_RS_TPL;
            let main_path = src_dir.join("main.rs");
            fs::write(&main_path, main_rs)?;
            
            println!("✅ Created Rust WASM project structure");
        }
        ShKind::Docker => {
            // Create Dockerfile
            let dockerfile = templates::DOCKERFILE_TPL;
            let dockerfile_path = Path::new(&path).join("Dockerfile");
            fs::write(&dockerfile_path, dockerfile)?;
            
            println!("✅ Created Dockerfile");
        }
        ShKind::Composition => {
            // Create composition template
            let composition = serde_json::json!({
                "name": name,
                "version": version,
                "steps": [],
                "wires": []
            });
            let composition_path = Path::new(&path).join("composition.json");
            fs::write(&composition_path, serde_json::to_string_pretty(&composition)?)?;
            
            println!("✅ Created composition template");
        }
    }

    Ok(())
}

pub async fn cmd_login_starthub(api_base: String) -> anyhow::Result<()> {
    println!("🔐 Logging in to StartHub...");
    println!("🌐 API Base: {}", api_base);
    
    // Open browser to editor for authentication
    let editor_url = "https://editor.starthub.so/cli-auth";
    println!("🌐 Opening browser to: {}", editor_url);
    
    match webbrowser::open(editor_url) {
        Ok(_) => println!("✅ Browser opened for authentication"),
        Err(e) => println!("⚠️  Could not open browser: {}. Please visit {}", e, editor_url),
    }
    
    // For now, just show success message
    println!("✅ Authentication flow initiated");
    println!("📝 Please complete authentication in your browser");
    
    Ok(())
}

pub async fn cmd_logout_starthub() -> anyhow::Result<()> {
    println!("🚪 Logging out from StartHub...");
    
    // Clear stored credentials
    let config_dir = dirs::config_dir().unwrap_or_else(|| std::env::temp_dir());
    let starthub_dir = config_dir.join("starthub");
    let token_file = starthub_dir.join("token");
    
    if token_file.exists() {
        fs::remove_file(&token_file)?;
        println!("✅ Authentication token removed");
    }
    
    println!("✅ Logged out successfully");
    Ok(())
}

pub async fn cmd_auth_status() -> anyhow::Result<()> {
    println!("🔍 Checking authentication status...");
    
    // Check for stored token
    let config_dir = dirs::config_dir().unwrap_or_else(|| std::env::temp_dir());
    let starthub_dir = config_dir.join("starthub");
    let token_file = starthub_dir.join("token");
    
    if token_file.exists() {
        println!("✅ Authenticated (token found)");
            } else {
        println!("❌ Not authenticated (no token found)");
        println!("💡 Run 'starthub login' to authenticate");
    }
    
    Ok(())
}

pub async fn cmd_start(bind: String) -> Result<()> {
    println!("🚀 Starting StartHub server in detached mode...");
    
    // Start the server as a detached process
    let server_process = start_server_process_detached(&bind).await?;
    
    // Wait a moment for server to start
    sleep(Duration::from_millis(2000)).await;
    
    println!("✅ Server started successfully!");
    println!("🌐 Server running at: http://{}", bind);
    println!("📝 Process ID: {}", server_process.id());
    println!("🔄 Server is running in the background");
    println!("💡 Use 'starthub run <action>' to interact with the server");
    println!("📋 Use 'starthub logs' to view server logs");
    println!("🛑 Use 'starthub stop' to stop the server");
    
    Ok(())
}

pub async fn cmd_stop() -> Result<()> {
    println!("🛑 Stopping StartHub server...");
    
    // Find and kill starthub-server processes
    let killed_count = kill_starthub_server_processes().await?;
    
    if killed_count > 0 {
        println!("✅ Stopped {} server process(es)", killed_count);
    } else {
        println!("ℹ️  No running StartHub server processes found");
    }
    
    Ok(())
}

pub async fn cmd_logs(follow: bool, lines: usize) -> Result<()> {
    // Get the log file path
    let log_file = get_server_log_file()?;
    
    if !log_file.exists() {
        println!("❌ Log file not found: {:?}", log_file);
        println!("💡 The server may not be running. Start it with 'starthub start'");
        return Ok(());
    }
    
    if follow {
        println!("📋 Following server logs (Press Ctrl+C to stop)...");
        println!("---");
        
        // First, show the last N lines
        let content = fs::read_to_string(&log_file)?;
        let all_lines: Vec<&str> = content.lines().collect();
        let start_line = all_lines.len().saturating_sub(lines);
        for line in all_lines.iter().skip(start_line) {
            println!("{}", line);
        }
        
        // Then follow new lines by polling the file
        let mut last_size = fs::metadata(&log_file)?.len();
        loop {
            sleep(Duration::from_millis(500)).await;
            
            let metadata = match fs::metadata(&log_file) {
                Ok(m) => m,
                Err(_) => continue, // File might have been deleted
            };
            
            if metadata.len() > last_size {
                // Read new content
                let file = tokio::fs::File::open(&log_file).await?;
                let mut reader = BufReader::new(file);
                
                // Seek to the last position we read
                reader.seek(tokio::io::SeekFrom::Start(last_size)).await?;
                
                let mut line = String::new();
                while reader.read_line(&mut line).await? > 0 {
                    print!("{}", line);
                    line.clear();
                }
                
                last_size = metadata.len();
            }
        }
    } else {
        // Just show the last N lines
        let content = fs::read_to_string(&log_file)?;
        let all_lines: Vec<&str> = content.lines().collect();
        let start_line = all_lines.len().saturating_sub(lines);
        
        println!("📋 Last {} lines of server logs:", lines);
        println!("---");
        for line in all_lines.iter().skip(start_line) {
            println!("{}", line);
        }
    }
    
    Ok(())
}

fn get_server_log_file() -> Result<std::path::PathBuf> {
    // Store log file in config directory
    let config_dir = dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .unwrap_or_else(|| std::env::temp_dir());
    let starthub_dir = config_dir.join("starthub");
    
    // Create directory if it doesn't exist
    fs::create_dir_all(&starthub_dir)?;
    
    Ok(starthub_dir.join("server.log"))
}

pub async fn cmd_run(action: String) -> Result<()> {
    // Parse the action argument to extract namespace, slug, and version
    let (namespace, slug, version) = parse_action_arg(&action);
    
    // Check if server is already running
    let server_running = check_server_running().await?;
    
    if !server_running {
        println!("🚀 Starting server...");
        // Start the server as a separate process
        let server_process = start_server_process().await?;
        
        // Wait a moment for server to start
        sleep(Duration::from_millis(2000)).await;
        
        println!("✅ Server started at {}", LOCAL_SERVER_URL);
    } else {
        println!("✅ Server already running at {}", LOCAL_SERVER_URL);
    }
    
    // Open browser to the server with a proper route for the Vue app
    let url = format!("{}/{}/{}/{}", LOCAL_SERVER_URL, namespace, slug, version);
    match webbrowser::open(&url) {
        Ok(_) => println!("↗ Opened browser to: {url}"),
        Err(e) => println!("→ Browser: {url} (couldn't auto-open: {e})"),
    }
    
    println!("📱 Serving UI for action: {} at route: {}", action, url);
    
    Ok(())
}

async fn check_server_running() -> Result<bool> {
    // Try to make a request to the server to see if it's running
    let client = reqwest::Client::new();
    let response = client
        .get(LOCAL_SERVER_URL)
        .timeout(Duration::from_millis(1000))
        .send()
        .await;
    
    match response {
        Ok(resp) => Ok(resp.status().is_success()),
        Err(_) => Ok(false),
    }
}

async fn start_server_process_detached(bind: &str) -> Result<std::process::Child> {
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
            Ok::<std::path::PathBuf, anyhow::Error>(current_dir.join(server_binary))
        })
        .or_else(|_| {
            // Try in the target/release directory
            Ok::<std::path::PathBuf, anyhow::Error>(std::env::current_dir()?.join("target").join("release").join(server_binary))
        })
        .or_else(|_| {
            // Try in the target/debug directory for development
            Ok::<std::path::PathBuf, anyhow::Error>(std::env::current_dir()?.join("target").join("debug").join(server_binary))
        })?;
    
    if !server_path.exists() {
        return Err(anyhow::anyhow!(
            "Server binary not found. Please build the server first with: cargo build --bin starthub-server"
        ));
    }
    
    println!("🚀 Starting server process: {:?}", server_path);
    
    // Get log file path
    let log_file = get_server_log_file()?;
    let log_file_path = log_file.as_path();
    
    // Open log file for writing (append mode)
    let log_file_handle = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file_path)?;
    
    // Start the server process in detached mode with stdout/stderr redirected to log file
    let child = std::process::Command::new(&server_path)
        .arg("--bind")
        .arg(bind)
        .stdout(Stdio::from(log_file_handle.try_clone()?))
        .stderr(Stdio::from(log_file_handle))
        .spawn()?;
    
    println!("📝 Server logs will be written to: {:?}", log_file_path);
    println!("💡 Use 'starthub logs' to view logs");
    
    Ok(child)
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
            Ok::<std::path::PathBuf, anyhow::Error>(current_dir.join(server_binary))
        })
        .or_else(|_| {
            // Try in the target/release directory
            Ok::<std::path::PathBuf, anyhow::Error>(std::env::current_dir()?.join("target").join("release").join(server_binary))
        })
        .or_else(|_| {
            // Try in the target/debug directory for development
            Ok::<std::path::PathBuf, anyhow::Error>(std::env::current_dir()?.join("target").join("debug").join(server_binary))
        })?;
    
    if !server_path.exists() {
        return Err(anyhow::anyhow!(
            "Server binary not found. Please build the server first with: cargo build --bin starthub-server"
        ));
    }
    
    println!("🚀 Starting server process: {:?}", server_path);
    
    // Start the server process
    let child = tokio::process::Command::new(&server_path)
        .arg("--bind")
        .arg(LOCAL_SERVER_HOST)
        .spawn()?;
    
    Ok(Some(child))
}

async fn kill_starthub_server_processes() -> Result<usize> {
    let mut killed_count = 0;
    
    #[cfg(unix)]
    {
        // Unix/Linux/macOS: Use ps and kill commands
        let output = std::process::Command::new("ps")
            .args(&["-ax", "-o", "pid,comm"])
            .output()?;
        
        if !output.status.success() {
            return Err(anyhow::anyhow!("Failed to list processes"));
        }
        
        let output_str = String::from_utf8_lossy(&output.stdout);
        
        for line in output_str.lines() {
            if line.contains("starthub-server") {
                let parts: Vec<&str> = line.trim().split_whitespace().collect();
                if let Some(pid_str) = parts.first() {
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        println!("🔍 Found starthub-server process: PID {}", pid);
                        
                        // Try to kill the process gracefully first
                        let kill_result = std::process::Command::new("kill")
                            .arg("-TERM")
                            .arg(pid.to_string())
                            .output();
                        
                        match kill_result {
                            Ok(output) => {
                                if output.status.success() {
                                    println!("✅ Killed process {}", pid);
                                    killed_count += 1;
                                } else {
                                    println!("⚠️  Failed to kill process {}: {}", pid, String::from_utf8_lossy(&output.stderr));
                                }
                            }
                            Err(e) => {
                                println!("⚠️  Failed to kill process {}: {}", pid, e);
                            }
                        }
                    }
                }
            }
        }
    }
    
    #[cfg(windows)]
    {
        // Windows: Use tasklist and taskkill commands
        let output = std::process::Command::new("tasklist")
            .args(&["/FI", "IMAGENAME eq starthub-server.exe", "/FO", "CSV"])
            .output()?;
        
        if !output.status.success() {
            return Err(anyhow::anyhow!("Failed to list processes"));
        }
        
        let output_str = String::from_utf8_lossy(&output.stdout);
        
        for line in output_str.lines() {
            if line.contains("starthub-server.exe") {
                let parts: Vec<&str> = line.split(',').collect();
                if parts.len() >= 2 {
                    let pid_str = parts[1].trim_matches('"');
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        println!("🔍 Found starthub-server process: PID {}", pid);
                        
                        // Try to kill the process
                        let kill_result = std::process::Command::new("taskkill")
                            .args(&["/PID", &pid.to_string(), "/F"])
                            .output();
                        
                        match kill_result {
                            Ok(output) => {
                                if output.status.success() {
                                    println!("✅ Killed process {}", pid);
                                    killed_count += 1;
                                } else {
                                    println!("⚠️  Failed to kill process {}: {}", pid, String::from_utf8_lossy(&output.stderr));
                                }
                            }
                            Err(e) => {
                                println!("⚠️  Failed to kill process {}: {}", pid, e);
                            }
                        }
                    }
                }
            }
        }
    }
    
    Ok(killed_count)
}

// Parse action argument in format "namespace/slug:version" or "namespace/slug@version" or "namespace/slug"
fn parse_action_arg(action: &str) -> (String, String, String) {
    // Default values
    let mut namespace = "tgirotto".to_string();
    let mut slug = "test-action".to_string();
    let mut version = "0.1.0".to_string();
    
    // Parse the action string - support both : and @ for version separator
    let version_separator = if action.contains(':') { ':' } else if action.contains('@') { '@' } else { '\0' };
    
    if version_separator != '\0' {
        if let Some(sep_pos) = action.find(version_separator) {
            let name_part = &action[..sep_pos];
            version = action[sep_pos + 1..].to_string();
            
            if let Some(slash_pos) = name_part.find('/') {
                namespace = name_part[..slash_pos].to_string();
                slug = name_part[slash_pos + 1..].to_string();
            } else {
                slug = name_part.to_string();
            }
        }
    } else if let Some(slash_pos) = action.find('/') {
        namespace = action[..slash_pos].to_string();
        slug = action[slash_pos + 1..].to_string();
    } else {
        slug = action.to_string();
    }
    
    (namespace, slug, version)
}

pub async fn cmd_status() -> Result<()> {
    println!("📊 Checking server status...");
    
    // Check if server process is running
    let processes = find_starthub_server_processes().await?;
    
    if processes.is_empty() {
        println!("❌ Server is not running");
        println!("💡 Start the server with 'starthub start'");
        return Ok(());
    }
    
    println!("✅ Server is running");
    println!("📋 Found {} server process(es):", processes.len());
    for (pid, cmd) in processes {
        println!("  - PID: {} | Command: {}", pid, cmd);
    }
    
    // Check if server is responding to HTTP requests
    let server_running = check_server_running().await?;
    if server_running {
        println!("🌐 Server is responding at {}", LOCAL_SERVER_URL);
    } else {
        println!("⚠️  Server process is running but not responding to HTTP requests");
        println!("💡 The server may still be starting up, or there may be an issue");
    }
    
    // Show log file location
    let log_file = get_server_log_file()?;
    if log_file.exists() {
        let metadata = std::fs::metadata(&log_file)?;
        let file_size = metadata.len();
        println!("📝 Log file: {:?} ({} bytes)", log_file, file_size);
    }
    
    Ok(())
}

async fn find_starthub_server_processes() -> Result<Vec<(u32, String)>> {
    let mut processes = Vec::new();
    
    #[cfg(unix)]
    {
        // Unix/Linux/macOS: Use ps command
        let output = std::process::Command::new("ps")
            .args(&["-ax", "-o", "pid,comm,args"])
            .output()?;
        
        if !output.status.success() {
            return Err(anyhow::anyhow!("Failed to list processes"));
        }
        
        let output_str = String::from_utf8_lossy(&output.stdout);
        
        for line in output_str.lines() {
            if line.contains("starthub-server") && !line.contains("grep") {
                let parts: Vec<&str> = line.trim().split_whitespace().collect();
                if let Some(pid_str) = parts.first() {
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        // Get the full command (everything after PID and comm)
                        let cmd_parts: Vec<&str> = parts.iter().skip(2).cloned().collect();
                        let cmd = cmd_parts.join(" ");
                        processes.push((pid, cmd));
                    }
                }
            }
        }
    }
    
    #[cfg(windows)]
    {
        // Windows: Use tasklist command
        let output = std::process::Command::new("tasklist")
            .args(&["/FI", "IMAGENAME eq starthub-server.exe", "/FO", "CSV", "/V"])
            .output()?;
        
        if !output.status.success() {
            return Err(anyhow::anyhow!("Failed to list processes"));
        }
        
        let output_str = String::from_utf8_lossy(&output.stdout);
        
        for line in output_str.lines() {
            if line.contains("starthub-server.exe") {
                let parts: Vec<&str> = line.split(',').collect();
                if parts.len() >= 2 {
                    let pid_str = parts[1].trim_matches('"');
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        let cmd = parts.iter().skip(1).collect::<Vec<_>>().join(" ");
                        processes.push((pid, cmd));
                    }
                }
            }
        }
    }
    
    Ok(processes)
}

/// Gets the ID of an existing action
async fn get_action_id(api_base: &str, action_name: &str, namespace: &str, access_token: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    
    // First get the owner ID for this namespace
    let owner_response = client
        .get(&format!("{}/rest/v1/owners?select=id&namespace=eq.{}", api_base, namespace))
        .header("Authorization", format!("Bearer {}", access_token))
        .header("apikey", access_token)
        .send()
        .await?;
    
    if owner_response.status().is_success() {
    let owners: Vec<serde_json::Value> = owner_response.json().await?;
        if let Some(owner) = owners.first() {
            if let Some(owner_id) = owner.get("id").and_then(|v| v.as_str()) {
    // Now get the action ID
                let action_response = client
                    .get(&format!("{}/rest/v1/actions?select=id&name=eq.{}&owner_id=eq.{}", api_base, action_name, owner_id))
        .header("Authorization", format!("Bearer {}", access_token))
                    .header("apikey", access_token)
        .send()
        .await?;
    
                if action_response.status().is_success() {
                    let actions: Vec<serde_json::Value> = action_response.json().await?;
        if let Some(action) = actions.first() {
                        if let Some(action_id) = action.get("id").and_then(|v| v.as_str()) {
                            return Ok(action_id.to_string());
                        }
                    }
                }
            }
        }
    }
    
    Err(anyhow::anyhow!("Action not found"))
}
