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

// Global constants for local development server
const LOCAL_SERVER_URL: &str = "http://127.0.0.1:3000";
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
    types_storage: Arc<std::sync::RwLock<HashMap<String, Value>>>,
    execution_orders: Arc<std::sync::RwLock<HashMap<String, Vec<String>>>>,
    composition_data: Arc<std::sync::RwLock<HashMap<String, serde_json::Value>>>,
}

impl AppState {
    fn new() -> Self {
        let (ws_sender, _) = broadcast::channel(100);
        Self { 
            ws_sender,
            types_storage: Arc::new(std::sync::RwLock::new(HashMap::new())),
            execution_orders: Arc::new(std::sync::RwLock::new(HashMap::new())),
            composition_data: Arc::new(std::sync::RwLock::new(HashMap::new())),
        }
    }
    
    /// Store types from a lock file in the global types storage
    fn store_types(&self, action_ref: &str, types: &HashMap<String, Value>) {
        if let Ok(mut storage) = self.types_storage.write() {
            for (type_name, type_schema) in types {
                let key = format!("{}:{}", action_ref, type_name);
                storage.insert(key, type_schema.clone());
                println!("📋 Stored type: {} from action: {}", type_name, action_ref);
            }
        }
    }
    
    /// Get all types for a specific action
    fn get_types_for_action(&self, action: &str) -> HashMap<String, Value> {
        if let Ok(storage) = self.types_storage.read() {
            storage.iter()
                .filter(|(key, _)| key.starts_with(&format!("{}:", action)))
                .map(|(key, value)| {
                    let type_name = key.split(':').last().unwrap_or(key);
                    (type_name.to_string(), value.clone())
                })
                .collect()
        } else {
            HashMap::new()
        }
    }
    
    /// Get all types across all actions
    fn get_all_types(&self) -> HashMap<String, Value> {
        if let Ok(storage) = self.types_storage.read() {
            storage.clone()
        } else {
            HashMap::new()
        }
    }
    
    /// Store execution order for an action
    fn store_execution_order(&self, action: &str, order: Vec<String>) {
        if let Ok(mut orders) = self.execution_orders.write() {
            orders.insert(action.to_string(), order);
            println!("📋 Stored execution order for action: {}", action);
        }
    }
    
    /// Get execution order for a specific action
    fn get_execution_order(&self, action: &str) -> Option<Vec<String>> {
        if let Ok(orders) = self.execution_orders.read() {
            orders.get(action).cloned()
        } else {
            None
        }
    }
    
    /// Get all execution orders
    fn get_all_execution_orders(&self) -> HashMap<String, Vec<String>> {
        if let Ok(orders) = self.execution_orders.read() {
            orders.clone()
        } else {
            HashMap::new()
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
        .route("/api/status", get(get_status))
        .route("/api/action", post(handle_action))
        .route("/api/run", post(handle_run))
        .route("/api/types", get(get_types))
        .route("/api/types/:action", get(get_types_for_action))
        .route("/api/execution-orders", get(get_execution_orders))
        .route("/api/execution-orders/:action", get(get_execution_order_for_action))
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

async fn get_status() -> Json<Value> {
    Json(json!({
        "status": "running",
        "timestamp": chrono::Utc::now().to_rfc3339()
    }))
}

async fn handle_action(
    axum::extract::State(state): axum::extract::State<AppState>,
    Json(payload): Json<Value>
) -> Json<Value> {
    // Handle the /api/action endpoint
    println!("📥 Action request: {:?}", payload);
    
    // For now, just echo back the payload
    Json(json!({
        "status": "received",
        "action": payload
    }))
}

async fn get_types(
    axum::extract::State(state): axum::extract::State<AppState>
) -> Json<Value> {
    let all_types = state.get_all_types();
    Json(json!({
        "types": all_types
    }))
}

async fn get_types_for_action(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(action): axum::extract::Path<String>
) -> Json<Value> {
    let types = state.get_types_for_action(&action);
    Json(json!({
        "action": action,
        "types": types
    }))
}

async fn get_execution_orders(
    axum::extract::State(state): axum::extract::State<AppState>
) -> Json<Value> {
    let all_orders = state.get_all_execution_orders();
    Json(json!({
        "execution_orders": all_orders
    }))
}

async fn get_execution_order_for_action(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(action): axum::extract::Path<String>
) -> Json<Value> {
    match state.get_execution_order(&action) {
        Some(order) => Json(json!({
            "action": action,
            "execution_order": order
        })),
        None => Json(json!({
            "action": action,
            "execution_order": null,
            "error": "No execution order found for this action"
        }))
    }
}

async fn handle_run(
    axum::extract::State(state): axum::extract::State<AppState>,
    Json(payload): Json<Value>
) -> Json<Value> {
    // Handle the /api/run endpoint that InputsComponent expects
    println!("📥 Run request: {:?}", payload);
    
    // Extract action and inputs from payload
    let action = payload.get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    
    let inputs = payload.get("inputs")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    
    println!("📥 Action: {}", action);
    println!("📥 Inputs: {:?}", inputs);
    
    // For now, just return a success response
    // In the future, this would trigger the actual execution
    Json(json!({
        "status": "success",
        "message": "Execution started",
        "action": action,
        "inputs": inputs
    }))
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
