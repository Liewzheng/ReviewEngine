//! In-memory log collection for the SSE logs endpoint.
//!
//! Captures tracing output via a custom `Write` implementation,
//! parses JSON log lines, and maintains a ring buffer of the last
//! 1000 entries. Also exposes a `tokio::sync::broadcast` channel
//! for real-time SSE streaming.

use std::io::Write;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

#[derive(Debug, Clone, serde::Serialize)]
pub struct LogEntry {
    pub id: String,
    pub timestamp: String,
    pub level: String,
    pub message: String,
    pub metadata: Option<LogMetadata>,
}

#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct LogMetadata {
    pub request_id: Option<String>,
    pub duration_ms: Option<u64>,
    pub review_id: Option<String>,
    pub expert_id: Option<String>,
}

pub struct LogCollector {
    buffer: Vec<u8>,
    entries: Vec<LogEntry>,
    tx: broadcast::Sender<LogEntry>,
    _rx: broadcast::Receiver<LogEntry>,
}

impl LogCollector {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(1000);
        Self {
            buffer: Vec::new(),
            entries: Vec::new(),
            tx,
            _rx,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LogEntry> {
        self.tx.subscribe()
    }

    pub fn recent_entries(&self, limit: usize) -> Vec<LogEntry> {
        let start = self.entries.len().saturating_sub(limit);
        self.entries[start..].to_vec()
    }

    fn add_bytes(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let line = String::from_utf8_lossy(&self.buffer[..pos]).to_string();
            self.buffer = self.buffer[pos + 1..].to_vec();
            self.parse_line(&line);
        }
        if self.buffer.len() > 4096 {
            self.buffer.clear();
        }
    }

    fn parse_line(&mut self, line: &str) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
            let level = json
                .get("level")
                .and_then(|v| v.as_str())
                .unwrap_or("INFO")
                .to_uppercase();
            let message = json
                .get("fields")
                .and_then(|v| v.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or(line)
                .to_string();
            let timestamp = json
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
            let entry = LogEntry {
                id: uuid::Uuid::new_v4().to_string(),
                timestamp,
                level,
                message,
                metadata: None,
            };
            self.entries.push(entry.clone());
            let _ = self.tx.send(entry);
        } else {
            let entry = LogEntry {
                id: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                level: "INFO".to_string(),
                message: line.to_string(),
                metadata: None,
            };
            self.entries.push(entry.clone());
            let _ = self.tx.send(entry);
        }
        if self.entries.len() > 1000 {
            self.entries.remove(0);
        }
    }
}

pub struct LogWriter {
    collector: Arc<Mutex<LogCollector>>,
}

impl LogWriter {
    pub fn new(collector: Arc<Mutex<LogCollector>>) -> Self {
        Self { collector }
    }
}

impl Clone for LogWriter {
    fn clone(&self) -> Self {
        Self {
            collector: self.collector.clone(),
        }
    }
}

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut c = self.collector.lock().unwrap();
        c.add_bytes(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

use std::sync::OnceLock;

static GLOBAL_COLLECTOR: OnceLock<Arc<Mutex<LogCollector>>> = OnceLock::new();

pub fn init_global_collector() -> Arc<Mutex<LogCollector>> {
    let collector = Arc::new(Mutex::new(LogCollector::new()));
    GLOBAL_COLLECTOR.set(collector.clone()).ok();
    collector
}

pub fn get_global_collector() -> Option<Arc<Mutex<LogCollector>>> {
    GLOBAL_COLLECTOR.get().cloned()
}
