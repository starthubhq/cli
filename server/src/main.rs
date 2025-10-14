use anyhow::Result;
use axum::{
    routing::{get, post},
    response::{Html, Json},
    Router,
    extract::ws::{WebSocketUpgrade, Message},
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

use starthub_server::{ execution};
use execution::ExecutionEngine;

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
}

impl AppState {
    fn new() -> Self {
        let (ws_sender, _) = broadcast::channel(100);
        
        // Initialize execution engine
        let execution_engine = ExecutionEngine::new(Some(ws_sender.clone()));
        let execution_engine = Arc::new(Mutex::new(execution_engine));
        
        Self { 
            ws_sender,
            execution_engine,
        }
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
    let state = AppState::new();
    
    // Get the UI directory path relative to the binary
    let ui_dir = get_ui_directory()?;
    let assets_dir = ui_dir.join("assets");
    
    // Create router with UI routes and API endpoints
    let app = Router::new()
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
    let possible_paths = vec![
        // When running from CLI directory (cargo run)
        std::env::current_dir()?.join("ui").join("dist"),
        // When running from server directory (cargo run from server/)
        std::env::current_dir()?.join("server").join("ui").join("dist"),
        // When running the binary directly from CLI directory
        binary_dir.join("ui").join("dist"),
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
    // Handle the /api/run endpoint that InputsComponent expects
    // Extract action and inputs from payload
    let action = payload.get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    
    // Parse inputs as JSON objects instead of strings
    let inputs = payload.get("inputs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    // If the item is a string, try to parse it as JSON
                    if let Some(json_str) = item.as_str() {
                        serde_json::from_str::<Value>(json_str).ok()
                    } else {
                        // If it's already a JSON object, use it directly
                        Some(item.clone())
                    }
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