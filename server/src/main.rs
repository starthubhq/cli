use anyhow::Result;
use axum::{
    routing::{get, post, patch},
    response::{Html, Json},
    Router,
    extract::{ws::{WebSocketUpgrade, Message}, Path},
    response::IntoResponse,
};
use tokio::net::TcpListener;
use tower_http::services::ServeDir;
use tower_http::cors::CorsLayer;
use serde_json::{Value, json};
use futures_util::{StreamExt, SinkExt};
use tokio::sync::broadcast;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::fs;
use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use starthub_server::{ execution, database};
use execution::ExecutionEngine;
use database::Database;
use uuid::Uuid;

// Global constants for local development server
const LOCAL_SERVER_HOST: &str = "127.0.0.1:3000";

#[derive(Parser, Debug)]
#[command(name="starthub-server", version, about="StartHub Local Server")]
struct ServerCli {
    /// Server host and port
    #[arg(long, default_value = LOCAL_SERVER_HOST)]
    bind: String,
    /// Verbose logs
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Clone)]
struct AppState {
    ws_sender: broadcast::Sender<String>,
    execution_engine: Arc<Mutex<ExecutionEngine>>,
    database: Arc<Mutex<Database>>,
}

impl AppState {
    fn new() -> Result<Self> {
        // Initialize execution engine
        let execution_engine = ExecutionEngine::new();
        let ws_sender = execution_engine.get_ws_sender().unwrap();
        let execution_engine = Arc::new(Mutex::new(execution_engine));
        
        // Initialize database
        let database = Database::new()?;
        let database = Arc::new(Mutex::new(database));
        
        Ok(Self { 
            ws_sender,
            execution_engine,
            database,
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = ServerCli::parse();

    let filter = if cli.verbose { "info" } else { "warn" };
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("STARTHUB_LOG").unwrap_or_else(|_| filter.into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    start_server(&cli.bind).await
}

async fn start_server(bind_addr: &str) -> Result<()> {
    // Create shared state
    let state = AppState::new()?;
    
    // Get the UI directory path relative to the binary
    let ui_dir = get_ui_directory()?;
    let assets_dir = ui_dir.join("assets");
    
    // Create router with UI routes and API endpoints
    let app = Router::new()
        .route("/api/actions", get(handle_get_actions).post(handle_create_action))
        .route("/api/actions/:id", get(handle_get_action))
        .route("/api/actions/:namespace/:slug/:version", get(handle_get_action_by_ref))
        .route("/api/actions/:id/versions/:version_id", patch(handle_update_version))
        .route("/api/run", post(handle_run))
        .route("/ws", get(ws_handler)) // WebSocket endpoint
        .nest_service("/assets", ServeDir::new(assets_dir))
        .nest_service("/favicon.ico", ServeDir::new(&ui_dir))
        .route("/", get(serve_index))
        .fallback(serve_spa) // SPA fallback for Vue Router
        .layer(CorsLayer::permissive())
        .with_state(state);

    // Start server
    let listener = TcpListener::bind(bind_addr).await?;
    println!("🌐 Server listening on http://{}", bind_addr);
    
    axum::serve(listener, app).await?;
    Ok(())
}

fn get_ui_directory() -> Result<std::path::PathBuf> {
    // Get the directory where the binary is located
    let current_exe = std::env::current_exe()?;
    let binary_dir = current_exe.parent().unwrap();
    
    // Try different possible locations for the UI directory
    // Priority: check binary directory first (for npm package installations)
    let possible_paths = vec![
        // When running from npm package (binary and UI in same directory)
        binary_dir.join("ui").join("dist"),
        // When running from CLI directory (cargo run)
        std::env::current_dir()?.join("ui").join("dist"),
        // When running from server directory (cargo run from server/)
        std::env::current_dir()?.join("server").join("ui").join("dist"),
        // When running from CLI directory with ./target/release/starthub-server
        std::env::current_dir()?.join("server").join("ui").join("dist"),
        // When running from target/release (go up to CLI, then to server/ui)
        binary_dir.join("..").join("..").join("server").join("ui").join("dist"),
        // When running from target/debug
        binary_dir.join("..").join("..").join("server").join("ui").join("dist"),
    ];
    
    for path in &possible_paths {
        if path.exists() && path.join("index.html").exists() {
            println!("📁 Found UI directory: {:?}", path);
            return Ok(path.clone());
        }
    }
    
    Err(anyhow::anyhow!("UI directory not found. Tried: {:?}", possible_paths))
}

async fn serve_index() -> Html<String> {
    // Read and serve the index.html file
    match get_ui_directory() {
        Ok(ui_dir) => {
            let index_path = ui_dir.join("index.html");
            match fs::read_to_string(&index_path) {
                Ok(content) => Html(content),
                Err(e) => {
                    println!("❌ Failed to read index.html from {:?}: {}", index_path, e);
                    Html("<!DOCTYPE html><html><body><h1>UI not found</h1><p>Make sure to build the UI first</p></body></html>".to_string())
                }
            }
        }
        Err(e) => {
            println!("❌ UI directory not found: {}", e);
            Html("<!DOCTYPE html><html><body><h1>UI not found</h1><p>Make sure to build the UI first</p></body></html>".to_string())
        }
    }
}

// SPA fallback - serve index.html for all routes to support Vue Router
async fn serve_spa() -> Html<String> {
    match get_ui_directory() {
        Ok(ui_dir) => {
            let index_path = ui_dir.join("index.html");
            match fs::read_to_string(&index_path) {
                Ok(content) => Html(content),
                Err(e) => {
                    println!("❌ Failed to read index.html from {:?}: {}", index_path, e);
                    Html("<!DOCTYPE html><html><body><h1>UI not found</h1><p>Make sure to build the UI first</p></body></html>".to_string())
                }
            }
        }
        Err(e) => {
            println!("❌ UI directory not found: {}", e);
            Html("<!DOCTYPE html><html><body><h1>UI not found</h1><p>Make sure to build the UI first</p></body></html>".to_string())
        }
    }
}

#[axum::debug_handler]
async fn handle_run(
    axum::extract::State(state): axum::extract::State<AppState>,
    Json(payload): Json<Value>
) -> Json<Value> {
    println!("payload: {:#?}", payload);
    // Handle the /api/run endpoint that InputsComponent expects
    // Extract action and inputs from payload
    let action = payload.get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    
    // Extract inputs array - values are already properly typed JSON values from the frontend
    let inputs = payload.get("inputs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|item| {
                    // Debug: Check if this looks like an SSH key and log its structure
                    if let Some(s) = item.as_str() {
                        if s.contains("BEGIN OPENSSH PRIVATE KEY") || s.contains("BEGIN RSA PRIVATE KEY") {
                            println!("🔑 Detected SSH key input: length={}, contains \\n: {}, contains actual newlines: {}", 
                                s.len(), 
                                s.contains("\\n"),
                                s.contains('\n'));
                        }
                    }
                    item.clone()  // Use values directly, they're already properly typed
                })
                .collect::<Vec<Value>>()
        })
        .unwrap_or_default();
    
    // Execute the action with array inputs
    let mut engine = state.execution_engine.lock().await;
    match engine.execute_action(action, inputs).await {
        Ok(result) => {
            // Send execution result via WebSocket
            let result_msg = json!({
                "type": "execution_complete",
                "action": action,
                "result": result,
                "timestamp": chrono::Utc::now().to_rfc3339()
            });
            
            if let Ok(msg_str) = serde_json::to_string(&result_msg) {
                let _ = state.ws_sender.send(msg_str);
            }
            
            Json(json!({
                "status": "success",
                "message": "Execution completed",
                "action": action,
                "result": result
            }))
        }
        Err(e) => {
            // Send error via WebSocket
            let error_msg = json!({
                "type": "execution_error",
                "action": action,
                "error": e.to_string(),
                "timestamp": chrono::Utc::now().to_rfc3339()
            });
            
            if let Ok(msg_str) = serde_json::to_string(&error_msg) {
                let _ = state.ws_sender.send(msg_str);
            }
            
            Json(json!({
                "status": "error",
                "message": "Execution failed",
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

async fn handle_ws(socket: axum::extract::ws::WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let ws_sender = state.ws_sender.clone();
    let mut ws_receiver = ws_sender.subscribe();
    
    // Send a welcome message
    let welcome_msg = json!({
        "type": "connection",
        "message": "Connected to Starthub WebSocket server",
        "timestamp": chrono::Utc::now().to_rfc3339()
    });
    
    if let Ok(msg) = serde_json::to_string(&welcome_msg) {
        let _ = sender.send(Message::Text(msg)).await;
    }

    // Spawn a task to forward broadcast messages to this WebSocket client
    let sender_clone = Arc::new(Mutex::new(sender));
    let sender_for_forward = sender_clone.clone();
    let forward_task = tokio::spawn(async move {
        while let Ok(msg) = ws_receiver.recv().await {
            let mut sender_guard = sender_for_forward.lock().await;
            if let Err(_) = sender_guard.send(Message::Text(msg)).await {
                break; // WebSocket closed
            }
        }
    });

    // Handle incoming messages from the client
    while let Some(msg) = receiver.next().await {
        if let Ok(msg) = msg {
            match msg {
                Message::Text(text) => {
                    // Echo back the message for now
                    let echo_msg = json!({
                        "type": "echo",
                        "message": text,
                        "timestamp": chrono::Utc::now().to_rfc3339()
                    });
                    
                    if let Ok(msg_str) = serde_json::to_string(&echo_msg) {
                        let mut sender_guard = sender_clone.lock().await;
                        let _ = sender_guard.send(Message::Text(msg_str)).await;
                    }
                }
                Message::Close(_) => {
                    break;
                }
                _ => {}
            }
        }
    }

    // Clean up the forward task
    forward_task.abort();
}

#[axum::debug_handler]
async fn handle_get_actions(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, axum::response::Response> {
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<i32>().ok())
        .or(Some(100));
    let namespace = params.get("namespace").map(|s| s.as_str());

    let db = state.database.lock().await;
    match db.get_actions_with_latest_version(limit, namespace) {
        Ok(actions) => {
            let response: Vec<Value> = actions
                .into_iter()
                .map(|av| {
                    json!({
                        "id": av.action.id,
                        "created_at": av.action.created_at,
                        "description": av.action.description,
                        "slug": av.action.slug,
                        "rls_owner_id": av.action.rls_owner_id,
                        "git_allowed_repository_id": av.action.git_allowed_repository_id,
                        "kind": av.action.kind,
                        "namespace": av.action.namespace,
                        "download_count": av.action.download_count,
                        "is_sync": av.action.is_sync,
                        "latest_action_version_id": av.action.latest_action_version_id,
                        "latest_version": av.latest_version.map(|v| json!({
                            "id": v.id,
                            "created_at": v.created_at,
                            "action_id": v.action_id,
                            "version_number": v.version_number,
                            "commit_sha": v.commit_sha,
                            "manifest": v.manifest,
                        })),
                    })
                })
                .collect();

            Ok(Json(json!(response)))
        }
        Err(e) => {
            Err(axum::response::Response::builder()
                .status(500)
                .body(axum::body::Body::from(format!("Database error: {}", e)))
                .unwrap()
                .into_response())
        }
    }
}

#[axum::debug_handler]
async fn handle_create_action(
    axum::extract::State(state): axum::extract::State<AppState>,
    Json(payload): Json<Value>
) -> Result<Json<Value>, axum::response::Response> {
    // Extract required fields
    let slug = payload.get("slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            axum::response::Response::builder()
                .status(400)
                .body(axum::body::Body::from("Missing required field: slug"))
                .unwrap()
                .into_response()
        })?;
    
    let kind = payload.get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("COMPOSITION");
    
    let description = payload.get("description")
        .and_then(|v| v.as_str());
    
    let namespace = payload.get("namespace")
        .and_then(|v| v.as_str());
    
    // Generate UUIDs for action and version
    let action_id = Uuid::new_v4().to_string();
    let version_id = Uuid::new_v4().to_string();
    let version_number = payload.get("version_number")
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.1");
    
    let db = state.database.lock().await;
    
    // Create the action
    match db.upsert_action(
        &action_id,
        slug,
        description,
        None, // rls_owner_id
        None, // git_allowed_repository_id
        kind,
        namespace,
        None, // latest_action_version_id - will be set after version creation
    ) {
        Ok(_) => {}
        Err(e) => {
            return Err(axum::response::Response::builder()
                .status(500)
                .body(axum::body::Body::from(format!("Failed to create action: {}", e)))
                .unwrap()
                .into_response());
        }
    }
    
    // Create initial action version
    let manifest = payload.get("manifest")
        .and_then(|v| serde_json::to_string(v).ok());
    
    match db.upsert_action_version(
        &version_id,
        &action_id,
        version_number,
        None, // commit_sha
        manifest.as_deref(),
    ) {
        Ok(_) => {}
        Err(e) => {
            return Err(axum::response::Response::builder()
                .status(500)
                .body(axum::body::Body::from(format!("Failed to create action version: {}", e)))
                .unwrap()
                .into_response());
        }
    }
    
    // Get the created action with version
    match db.get_action(&action_id) {
        Ok(Some(action)) => {
            let latest_version = db.get_latest_action_version(&action_id).ok().flatten();
            
            Ok(Json(json!({
                "id": action.id,
                "created_at": action.created_at,
                "description": action.description,
                "slug": action.slug,
                "rls_owner_id": action.rls_owner_id,
                "git_allowed_repository_id": action.git_allowed_repository_id,
                "kind": action.kind,
                "namespace": action.namespace,
                "download_count": action.download_count,
                "is_sync": action.is_sync,
                "latest_action_version_id": action.latest_action_version_id,
                "latest_version": latest_version.map(|v| json!({
                    "id": v.id,
                    "created_at": v.created_at,
                    "action_id": v.action_id,
                    "version_number": v.version_number,
                    "commit_sha": v.commit_sha,
                    "manifest": v.manifest,
                })),
            })))
        }
        Ok(None) => {
            Err(axum::response::Response::builder()
                .status(404)
                .body(axum::body::Body::from("Action not found after creation"))
                .unwrap()
                .into_response())
        }
        Err(e) => {
            Err(axum::response::Response::builder()
                .status(500)
                .body(axum::body::Body::from(format!("Failed to retrieve created action: {}", e)))
                .unwrap()
                .into_response())
        }
    }
}

#[axum::debug_handler]
async fn handle_get_action(
    axum::extract::State(state): axum::extract::State<AppState>,
    Path(action_id): Path<String>,
) -> Result<Json<Value>, axum::response::Response> {
    let db = state.database.lock().await;
    match db.get_action(&action_id) {
        Ok(Some(action)) => {
            let latest_version = db.get_latest_action_version(&action_id).ok().flatten();
            
            Ok(Json(json!({
                "id": action.id,
                "created_at": action.created_at,
                "description": action.description,
                "slug": action.slug,
                "rls_owner_id": action.rls_owner_id,
                "git_allowed_repository_id": action.git_allowed_repository_id,
                "kind": action.kind,
                "namespace": action.namespace,
                "download_count": action.download_count,
                "is_sync": action.is_sync,
                "latest_action_version_id": action.latest_action_version_id,
                "latest_version": latest_version.map(|v| json!({
                    "id": v.id,
                    "created_at": v.created_at,
                    "action_id": v.action_id,
                    "version_number": v.version_number,
                    "commit_sha": v.commit_sha,
                    "manifest": v.manifest,
                })),
            })))
        }
        Ok(None) => {
            Err(axum::response::Response::builder()
                .status(404)
                .body(axum::body::Body::from("Action not found"))
                .unwrap()
                .into_response())
        }
        Err(e) => {
            Err(axum::response::Response::builder()
                .status(500)
                .body(axum::body::Body::from(format!("Database error: {}", e)))
                .unwrap()
                .into_response())
        }
    }
}

#[axum::debug_handler]
async fn handle_get_action_by_ref(
    axum::extract::State(state): axum::extract::State<AppState>,
    Path((namespace, slug, version)): Path<(String, String, String)>,
) -> Result<Json<Value>, axum::response::Response> {
    let db = state.database.lock().await;
    
    // Handle empty namespace (could be empty string or "null")
    let namespace_opt = if namespace.is_empty() || namespace == "null" {
        None
    } else {
        Some(namespace.as_str())
    };
    
    match db.get_action_by_namespace_slug(
        namespace_opt.unwrap_or(""),
        &slug,
    ) {
        Ok(Some(action)) => {
            // Get all versions for this action
            let versions = db.get_action_versions(&action.id).unwrap_or_default();
            
            // Find the version that matches the requested version number
            let requested_version = versions.iter()
                .find(|v| v.version_number == version);
            
            if requested_version.is_none() {
                return Err(axum::response::Response::builder()
                    .status(404)
                    .body(axum::body::Body::from(format!("Version '{}' not found for action", version)))
                    .unwrap()
                    .into_response());
            }
            
            let version_record = requested_version.unwrap();
            
            Ok(Json(json!({
                "id": action.id,
                "created_at": action.created_at,
                "description": action.description,
                "slug": action.slug,
                "rls_owner_id": action.rls_owner_id,
                "git_allowed_repository_id": action.git_allowed_repository_id,
                "kind": action.kind,
                "namespace": action.namespace,
                "download_count": action.download_count,
                "is_sync": action.is_sync,
                "latest_action_version_id": action.latest_action_version_id,
                "version": {
                    "id": version_record.id,
                    "created_at": version_record.created_at,
                    "action_id": version_record.action_id,
                    "version_number": version_record.version_number,
                    "commit_sha": version_record.commit_sha,
                    "manifest": version_record.manifest,
                },
            })))
        }
        Ok(None) => {
            Err(axum::response::Response::builder()
                .status(404)
                .body(axum::body::Body::from("Action not found"))
                .unwrap()
                .into_response())
        }
        Err(e) => {
            Err(axum::response::Response::builder()
                .status(500)
                .body(axum::body::Body::from(format!("Database error: {}", e)))
                .unwrap()
                .into_response())
        }
    }
}

#[axum::debug_handler]
async fn handle_update_version(
    axum::extract::State(state): axum::extract::State<AppState>,
    Path((action_id, version_id)): Path<(String, String)>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, axum::response::Response> {
    // Extract manifest from payload
    let manifest = payload.get("manifest")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let db = state.database.lock().await;
    
    // Get the version to ensure it exists and belongs to the action
    match db.get_action_versions(&action_id) {
        Ok(versions) => {
            let version = versions.iter().find(|v| v.id == version_id);
            if version.is_none() {
                return Err(axum::response::Response::builder()
                    .status(404)
                    .body(axum::body::Body::from("Version not found"))
                    .unwrap()
                    .into_response());
            }
            
            let version = version.unwrap();
            
            // Update the version with the new manifest
            match db.upsert_action_version(
                &version_id,
                &action_id,
                &version.version_number,
                version.commit_sha.as_deref(),
                manifest.as_deref(),
            ) {
                Ok(_) => {
                    // Get the updated version
                    match db.get_latest_action_version(&action_id) {
                        Ok(Some(updated_version)) => {
                            Ok(Json(json!({
                                "id": updated_version.id,
                                "created_at": updated_version.created_at,
                                "action_id": updated_version.action_id,
                                "version_number": updated_version.version_number,
                                "commit_sha": updated_version.commit_sha,
                                "manifest": updated_version.manifest,
                            })))
                        }
                        _ => {
                            // Fallback: return success even if we can't fetch the updated version
                            Ok(Json(json!({
                                "id": version_id,
                                "status": "updated"
                            })))
                        }
                    }
                }
                Err(e) => {
                    Err(axum::response::Response::builder()
                        .status(500)
                        .body(axum::body::Body::from(format!("Failed to update version: {}", e)))
                        .unwrap()
                        .into_response())
                }
            }
        }
        Err(e) => {
            Err(axum::response::Response::builder()
                .status(500)
                .body(axum::body::Body::from(format!("Database error: {}", e)))
                .unwrap()
                .into_response())
        }
    }
}