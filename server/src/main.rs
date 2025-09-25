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
use std::collections::HashMap;
use std::fs;
use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod models;
mod execution;

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
    execution_engine: Arc<ExecutionEngine>,
}

impl AppState {
    fn new() -> Self {
        let (ws_sender, _) = broadcast::channel(100);
        
        // Initialize execution engine with API configuration
        let base_url = std::env::var("STARTHUB_API").unwrap_or_else(|_| "https://api.starthub.so".to_string());
        let token = std::env::var("STARTHUB_TOKEN").ok();
        let execution_engine = Arc::new(ExecutionEngine::new(base_url, token));
        
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
    
    // Create router with UI routes and API endpoints
    let app = Router::new()
        .route("/api/run", post(handle_run))
        .route("/ws", get(ws_handler)) // WebSocket endpoint
        .nest_service("/assets", ServeDir::new("ui/dist/assets"))
        .nest_service("/favicon.ico", ServeDir::new("ui/dist"))
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

async fn handle_run(
    axum::extract::State(state): axum::extract::State<AppState>,
    Json(payload): Json<Value>
) -> Json<Value> {
    // Handle the /api/run endpoint that InputsComponent expects
    // Extract action and inputs from payload
    let action = payload.get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    
    let inputs = payload.get("inputs")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    
    // Convert inputs to HashMap<String, Value>
    let mut input_map = HashMap::new();
    for (key, value) in inputs {
        input_map.insert(key, value);
    }
    
    // Execute the action
    match state.execution_engine.execute_action(action, input_map).await {
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