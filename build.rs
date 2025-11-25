use std::process::Command;
use std::path::Path;
use std::fs;

fn main() {
    println!("cargo:rerun-if-changed=console/package.json");
    println!("cargo:rerun-if-changed=console/vite.config.ts");
    println!("cargo:rerun-if-changed=console/src");
    
    // Check if UI dist already exists (e.g., from CI build)
    let server_ui_dist = Path::new("server/ui/dist");
    if server_ui_dist.exists() && server_ui_dist.join("index.html").exists() {
        println!("cargo:warning=UI dist already exists, skipping console build");
        return;
    }
    
    // Build the console
    println!("cargo:warning=Building console...");
    let console_dir = Path::new("console");
    
    // Run npm install if node_modules doesn't exist
    if !console_dir.join("node_modules").exists() {
        println!("cargo:warning=Installing console dependencies...");
        let install_status = Command::new("npm")
            .arg("install")
            .current_dir(&console_dir)
            .status();
        
        if let Err(e) = install_status {
            panic!("Failed to run npm install in console: {}", e);
        }
    }
    
    // Run npm run build
    println!("cargo:warning=Running npm run build in console...");
    let build_status = Command::new("npm")
        .arg("run")
        .arg("build")
        .current_dir(&console_dir)
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

