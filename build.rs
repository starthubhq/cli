use std::process::Command;
use std::path::Path;
use std::fs;

fn main() {
    println!("cargo:rerun-if-changed=console/package.json");
    println!("cargo:rerun-if-changed=console/vite.config.ts");
    println!("cargo:rerun-if-changed=console/src");
    
    // Build the console
    println!("cargo:warning=Building console...");
    let console_dir = Path::new("console");
    
    // Determine npm command based on platform
    // On Windows, npm is typically npm.cmd, but npm should also work if PATH is set correctly
    let npm_cmd = if cfg!(target_os = "windows") {
        // Try npm.cmd first, then fall back to npm
        let npm_cmd_check = Command::new("npm.cmd")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        
        if npm_cmd_check.is_ok() {
            "npm.cmd"
        } else {
            "npm"
        }
    } else {
        "npm"
    };
    
    // Check if npm is available
    let npm_check = Command::new(npm_cmd)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    
    if let Err(e) = npm_check {
        panic!(
            "npm is not available: {}\n\
            Please ensure Node.js and npm are installed and available in PATH.\n\
            In CI environments, make sure the 'Setup Node.js' step runs before the build step.\n\
            On Windows, ensure npm is accessible via PATH or use npm.cmd.",
            e
        );
    }
    
    // Run npm install if node_modules doesn't exist
    if !console_dir.join("node_modules").exists() {
        println!("cargo:warning=Installing console dependencies...");
        let install_status = Command::new(npm_cmd)
            .arg("install")
            .current_dir(&console_dir)
            .status();
        
        if let Err(e) = install_status {
            panic!("Failed to run npm install in console: {}", e);
        }
    }
    
    // Run npm run build with Supabase environment variables
    println!("cargo:warning=Running npm run build in console...");
    let build_status = Command::new(npm_cmd)
        .arg("run")
        .arg("build")
        .current_dir(&console_dir)
        .env("VITE_SUPABASE_URL", "https://smltnjrrzkmazvbrqbkq.supabase.co")
        .env("VITE_SUPABASE_ANON_KEY", "sb_publishable_AKGy20M54_uMOdJme3ZnZA_GX11LgHe")
        .status();
    
    match build_status {
        Ok(status) if status.success() => {
            println!("cargo:warning=Console build successful");
        }
        Ok(status) => {
            panic!("Console build failed with exit code: {:?}", status.code());
        }
        Err(e) => {
            panic!("Failed to run npm run build in console: {}", e);
        }
    }
    
    // Copy dist folder from console to server/ui
    let console_dist = console_dir.join("dist");
    let server_ui_dist = Path::new("server/ui/dist");
    
    if !console_dist.exists() {
        panic!("Console dist folder not found at: {:?}", console_dist);
    }
    
    // Create server/ui/dist directory if it doesn't exist
    if let Err(e) = fs::create_dir_all(&server_ui_dist) {
        panic!("Failed to create server/ui/dist directory: {}", e);
    }
    
    // Remove existing dist contents if it exists
    if server_ui_dist.exists() {
        if let Err(e) = fs::remove_dir_all(&server_ui_dist) {
            panic!("Failed to remove existing server/ui/dist: {}", e);
        }
    }
    
    // Copy all files from console/dist to server/ui/dist
    println!("cargo:warning=Copying console/dist to server/ui/dist...");
    copy_dir_all(&console_dist, &server_ui_dist)
        .expect("Failed to copy console/dist to server/ui/dist");
    
    println!("cargo:warning=Console build and copy completed successfully");
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let dst_path = dst.join(&file_name);
        
        if path.is_dir() {
            copy_dir_all(&path, &dst_path)?;
        } else {
            fs::copy(&path, &dst_path)?;
        }
    }
    
    Ok(())
}

