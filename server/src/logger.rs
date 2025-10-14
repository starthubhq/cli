use serde_json::json;
use tokio::sync::broadcast;
use chrono;

/// Logger struct that handles all logging functionality
pub struct Logger {
    ws_sender: Option<broadcast::Sender<String>>,
}

impl Logger {
    /// Create a new logger instance
    pub fn new() -> Self {
        Self {
            ws_sender: None,
        }
    }

    /// Create a new logger instance with WebSocket sender
    pub fn new_with_ws_sender(ws_sender: Option<broadcast::Sender<String>>) -> Self {
        Self {
            ws_sender,
        }
    }

    /// Set the WebSocket sender for real-time logging
    pub fn set_ws_sender(&mut self, sender: broadcast::Sender<String>) {
        self.ws_sender = Some(sender);
    }

    /// Get the WebSocket sender
    pub fn get_ws_sender(&self) -> Option<broadcast::Sender<String>> {
        self.ws_sender.clone()
    }

    /// Core logging function that sends messages via WebSocket
    pub fn log(&self, level: &str, message: &str, action_id: Option<&str>) {
        if let Some(sender) = &self.ws_sender {
            let log_msg = json!({
                "type": "log",
                "level": level,
                "message": message,
                "action_id": action_id,
                "timestamp": chrono::Utc::now().to_rfc3339()
            });
            
            if let Ok(msg_str) = serde_json::to_string(&log_msg) {
                let _ = sender.send(msg_str);
            }
        }
    }

    /// Log an info message
    pub fn log_info(&self, message: &str, action_id: Option<&str>) {
        self.log("info", message, action_id);
    }

    /// Log an error message
    pub fn log_error(&self, message: &str, action_id: Option<&str>) {
        self.log("error", message, action_id);
    }

    /// Log a success message
    pub fn log_success(&self, message: &str, action_id: Option<&str>) {
        self.log("success", message, action_id);
    }

    /// Log a warning message
    pub fn log_warning(&self, message: &str, action_id: Option<&str>) {
        self.log("warning", message, action_id);
    }

    /// Log a debug message
    pub fn log_debug(&self, message: &str, action_id: Option<&str>) {
        self.log("debug", message, action_id);
    }
}

/// Trait for objects that can log messages
pub trait Loggable {
    fn log_info(&self, message: &str, action_id: Option<&str>);
    fn log_error(&self, message: &str, action_id: Option<&str>);
    fn log_success(&self, message: &str, action_id: Option<&str>);
    fn log_warning(&self, message: &str, action_id: Option<&str>);
    fn log_debug(&self, message: &str, action_id: Option<&str>);
}

impl Loggable for Logger {
    fn log_info(&self, message: &str, action_id: Option<&str>) {
        self.log_info(message, action_id);
    }

    fn log_error(&self, message: &str, action_id: Option<&str>) {
        self.log_error(message, action_id);
    }

    fn log_success(&self, message: &str, action_id: Option<&str>) {
        self.log_success(message, action_id);
    }

    fn log_warning(&self, message: &str, action_id: Option<&str>) {
        self.log_warning(message, action_id);
    }

    fn log_debug(&self, message: &str, action_id: Option<&str>) {
        self.log_debug(message, action_id);
    }
}
