fn main() {
    let action_ref = "starthubhq/http-get-wasm:0.0.16";
    let url_path = action_ref.replace(":", "/");
    let storage_url = format!(
        "{}{}/{}/artifact.zip",
        "https://api.starthub.so",
        "/storage/v1/object/public/artifacts",
        url_path
    );
    println!("Constructed URL: {}", storage_url);
    println!("Expected URL: https://api.starthub.so/storage/v1/object/public/artifacts/starthubhq/http-get-wasm/0.0.16/artifact.zip");
}
