//! In-memory log collection for the SSE logs endpoint.
//!
//! Captures tracing output via a custom `Write` implementation,
//! parses JSON log lines, and maintains a ring buffer of the last
//! 1000 entries. Also exposes a `tokio::sync::broadcast` channel
//! for real-time SSE streaming.

use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// A parsed log entry ready for API consumption and SSE broadcast.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogEntry {
    /// Unique identifier for this log entry (UUID v4).
    pub id: String,
    /// ISO 8601 timestamp when the log was recorded.
    pub timestamp: String,
    /// Log level: `INFO`, `WARN`, `ERROR`, `DEBUG`, or `TRACE`.
    pub level: String,
    /// Human-readable log message text.
    pub message: String,
    /// Optional structured metadata (request ID, duration, review ID, expert ID).
    pub metadata: Option<LogMetadata>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LogMetadata {
    /// `requestId` on the wire (frontend `types/logs.ts`); the snake_case
    /// aliases keep legacy `logs.ndjson` files readable on load.
    #[serde(alias = "request_id")]
    pub request_id: Option<String>,
    #[serde(alias = "duration_ms")]
    pub duration_ms: Option<u64>,
    #[serde(alias = "review_id")]
    pub review_id: Option<String>,
    #[serde(alias = "expert_id")]
    pub expert_id: Option<String>,
}

/// In-memory log collector with ring buffer, SSE broadcast, and NDJSON persistence.
///
/// Captures tracing output via [`LogWriter`] (a `std::io::Write` impl),
/// parses both JSON and plain-text log lines, and maintains a ring buffer
/// of the last 1000 entries. Entries are broadcast via
/// `tokio::sync::broadcast` for real-time SSE streaming and appended to
/// an NDJSON file for persistence across restarts.
pub struct LogCollector {
    buffer: Vec<u8>,
    entries: Vec<LogEntry>,
    tx: broadcast::Sender<LogEntry>,
    _rx: broadcast::Receiver<LogEntry>,
    #[allow(dead_code)]
    file_path: Option<PathBuf>,
    file: Option<std::fs::File>,
}

impl LogCollector {
    pub fn new() -> Self {
        Self::new_with_path(default_ndjson_path())
    }

    pub fn new_with_path(file_path: Option<PathBuf>) -> Self {
        let (tx, _rx) = broadcast::channel(1000);
        let mut collector = Self {
            buffer: Vec::new(),
            entries: Vec::new(),
            tx,
            _rx,
            file_path: file_path.clone(),
            file: None,
        };
        if let Some(path) = file_path {
            if let Some(parent) = path.parent() {
                if !parent.exists() {
                    let _ = std::fs::create_dir_all(parent);
                }
            }
            if path.exists() {
                if let Err(e) = collector.load_from_file(&path) {
                    eprintln!("Failed to load log history from {:?}: {}", path, e);
                }
            }
            match OpenOptions::new().create(true).append(true).open(&path) {
                Ok(f) => collector.file = Some(f),
                Err(e) => eprintln!("Failed to open log file {:?} for appending: {}", path, e),
            }
        }
        collector
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

    fn load_from_file(&mut self, path: &PathBuf) -> std::io::Result<()> {
        let file = std::fs::File::open(path)?;
        let reader = std::io::BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<LogEntry>(&line) {
                self.entries.push(entry);
            }
        }
        if self.entries.len() > 1000 {
            let remove_count = self.entries.len() - 1000;
            self.entries.drain(0..remove_count);
        }
        Ok(())
    }

    fn append_entry_to_file(&mut self, entry: &LogEntry) {
        if let Some(file) = &mut self.file {
            if let Ok(line) = serde_json::to_string(entry) {
                if writeln!(file, "{}", line).is_err() {
                    // Silently ignore write failures to avoid disrupting log collection
                }
            }
        }
    }

    fn parse_line(&mut self, line: &str) {
        let entry = if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
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
            LogEntry {
                id: uuid::Uuid::new_v4().to_string(),
                timestamp,
                level,
                message,
                metadata: None,
            }
        } else {
            LogEntry {
                id: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                level: infer_level_from_line(line),
                message: line.to_string(),
                metadata: None,
            }
        };
        self.record_entry(entry);
    }

    /// Persist an entry — parsed or programmatically constructed — through the
    /// single pipeline: ring buffer, SSE broadcast, NDJSON file, ring trim.
    fn record_entry(&mut self, entry: LogEntry) {
        self.entries.push(entry.clone());
        let _ = self.tx.send(entry.clone());
        self.append_entry_to_file(&entry);
        if self.entries.len() > 1000 {
            self.entries.remove(0);
        }
    }

    /// Record a structured entry carrying optional [`LogMetadata`], bypassing
    /// the text/JSON parse path. Key lifecycle points (review started /
    /// completed / failed) use this so `LogMetadata` fields are populated at
    /// the source instead of always being `None`.
    pub fn push_entry(&mut self, level: &str, message: impl Into<String>, metadata: Option<LogMetadata>) {
        let entry = LogEntry {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            level: level.to_uppercase(),
            message: message.into(),
            metadata,
        };
        self.record_entry(entry);
    }
}

/// Infer the tracing level from a plain-text (non-JSON) log line.
///
/// The `LogWriter` receives tracing's pre-formatted text, so the level token
/// (e.g. ` 2026-01-01T00:00:00Z  WARN crate: ...`) is only recoverable by
/// scanning the line. Unknown lines default to `INFO`.
fn infer_level_from_line(line: &str) -> String {
    let upper = line.to_uppercase();
    for level in ["ERROR", "WARN", "DEBUG", "TRACE"] {
        if upper.contains(level) {
            return level.to_string();
        }
    }
    "INFO".to_string()
}

/// `std::io::Write` adapter that feeds bytes into a [`LogCollector`].
///
/// Used as the `tracing` subscriber's writer so all tracing output is
/// captured into the collector's ring buffer and broadcast channel.
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

/// Initialise the global log collector with the default NDJSON path.
///
/// Returns the shared `Arc<Mutex<LogCollector>>` and stores it in the
/// process-global `OnceLock`. Subsequent calls return the same instance.
pub fn init_global_collector() -> Arc<Mutex<LogCollector>> {
    init_global_collector_with_path(default_ndjson_path())
}

pub fn init_global_collector_with_path(path: Option<PathBuf>) -> Arc<Mutex<LogCollector>> {
    let collector = Arc::new(Mutex::new(LogCollector::new_with_path(path)));
    GLOBAL_COLLECTOR.set(collector.clone()).ok();
    collector
}

/// Retrieve the global log collector instance, if initialised.
///
/// Returns `None` when the collector has not been set up (e.g. unit tests
/// that skip `init_global_collector`).
pub fn get_global_collector() -> Option<Arc<Mutex<LogCollector>>> {
    GLOBAL_COLLECTOR.get().cloned()
}

/// Push a structured lifecycle entry onto the global collector.
///
/// No-op when the collector has not been initialised (e.g. lib unit tests that
/// build `AppState` directly without going through `main`), so review-task
/// lifecycle points can log unconditionally.
pub fn push_global_entry(level: &str, message: String, metadata: Option<LogMetadata>) {
    if let Some(collector) = get_global_collector() {
        if let Ok(mut guard) = collector.lock() {
            guard.push_entry(level, message, metadata);
        }
    }
}

/// Default NDJSON log file location (`~/.config/review-engine/logs.ndjson`).
/// `None` when the home directory cannot be determined (file logging off).
pub fn default_ndjson_path() -> Option<PathBuf> {
    home::home_dir().map(|p| p.join(".config").join("review-engine").join("logs.ndjson"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit 11: the plain-text (non-JSON) branch infers the level from the
    /// line instead of hardcoding INFO.
    #[test]
    fn infer_level_from_plain_text_lines() {
        assert_eq!(infer_level_from_line("2026-01-01T00:00:00Z  INFO x: hi"), "INFO");
        assert_eq!(infer_level_from_line("2026-01-01T00:00:00Z  WARN x: slow"), "WARN");
        assert_eq!(infer_level_from_line("ERROR: boom"), "ERROR");
        assert_eq!(infer_level_from_line("2026-01-01T00:00:00Z  DEBUG x: trace"), "DEBUG");
        assert_eq!(infer_level_from_line("2026-01-01T00:00:00Z  TRACE x: detail"), "TRACE");
        assert_eq!(infer_level_from_line("plain line without level"), "INFO");
    }

    #[test]
    fn parse_line_records_inferred_level() {
        let mut c = LogCollector::new_with_path(None);
        c.add_bytes(b"2026-01-01T00:00:00Z  WARN module: something is off\n");
        let entries = c.recent_entries(10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, "WARN");
        assert!(entries[0].message.contains("something is off"));
    }

    /// Unit 14: `push_entry` populates `LogMetadata` at the source (instead of
    /// the always-`None` parse path) and the entry serializes camelCase so the
    /// frontend badge renders.
    #[test]
    fn push_entry_carries_metadata_and_serializes_camelcase() {
        let mut c = LogCollector::new_with_path(None);
        c.push_entry(
            "info",
            "review task started",
            Some(LogMetadata {
                request_id: Some("req-1".to_string()),
                duration_ms: Some(42),
                review_id: Some("rev-1".to_string()),
                expert_id: Some("exp-1".to_string()),
            }),
        );
        let entries = c.recent_entries(10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, "INFO", "level must be normalized uppercase");
        assert_eq!(entries[0].message, "review task started");

        let meta = entries[0]
            .metadata
            .as_ref()
            .expect("push_entry metadata must not be None");
        assert_eq!(meta.request_id.as_deref(), Some("req-1"));
        assert_eq!(meta.duration_ms, Some(42));
        assert_eq!(meta.review_id.as_deref(), Some("rev-1"));
        assert_eq!(meta.expert_id.as_deref(), Some("exp-1"));

        let json = serde_json::to_value(&entries[0]).unwrap();
        assert_eq!(json["metadata"]["requestId"], "req-1");
        assert_eq!(json["metadata"]["durationMs"], 42);
        assert_eq!(json["metadata"]["reviewId"], "rev-1");
        assert_eq!(json["metadata"]["expertId"], "exp-1");
    }

    /// Unit 15: the parse path still records `metadata: None` (existing stream
    /// unchanged), while a `push_entry` lifecycle record in the same buffer
    /// carries metadata — the two sources coexist in one ring buffer.
    #[test]
    fn parsed_lines_keep_metadata_none_while_pushed_entries_carry_metadata() {
        let mut c = LogCollector::new_with_path(None);
        c.add_bytes(b"2026-01-01T00:00:00Z  INFO x: parsed line\n");
        c.push_entry(
            "ERROR",
            "review task failed".to_string(),
            Some(LogMetadata {
                request_id: Some("req-9".to_string()),
                duration_ms: Some(7),
                review_id: Some("rev-9".to_string()),
                expert_id: None,
            }),
        );
        let entries = c.recent_entries(10);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].metadata.is_none(), "parsed lines keep metadata None");
        assert!(entries[1].metadata.is_some(), "pushed lifecycle entries carry metadata");
        assert_eq!(entries[1].level, "ERROR");
    }

    /// Unit 16: a pushed entry round-trips through the NDJSON file — reloading
    /// from the file preserves the camelCase metadata fields via aliases.
    #[test]
    fn pushed_entry_round_trips_through_ndjson_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs.ndjson");
        {
            let mut c = LogCollector::new_with_path(Some(path.clone()));
            c.push_entry(
                "INFO",
                "review completed",
                Some(LogMetadata {
                    request_id: Some("req-r".to_string()),
                    duration_ms: Some(123),
                    review_id: Some("rev-r".to_string()),
                    expert_id: None,
                }),
            );
        } // collector dropped; file flushed by drop? file handle closed on drop
        let c2 = LogCollector::new_with_path(Some(path.clone()));
        let entries = c2.recent_entries(10);
        assert_eq!(entries.len(), 1, "reloaded NDJSON must contain the pushed entry");
        let meta = entries[0].metadata.as_ref().expect("metadata survives reload");
        assert_eq!(meta.request_id.as_deref(), Some("req-r"));
        assert_eq!(meta.duration_ms, Some(123));
        assert_eq!(meta.review_id.as_deref(), Some("rev-r"));
    }

    /// Unit 13: `LogMetadata` serializes camelCase (`requestId`/`durationMs`/
    /// `reviewId`/`expertId`) to match `frontend/src/types/logs.ts`, so SSE and
    /// download (which share the same serializer) render the badge metadata.
    /// Legacy snake_case lines still deserialize via aliases.
    #[test]
    fn log_metadata_serializes_camelcase() {
        let meta = LogMetadata {
            request_id: Some("req-1".to_string()),
            duration_ms: Some(12),
            review_id: Some("rev-1".to_string()),
            expert_id: Some("exp-1".to_string()),
        };
        let json = serde_json::to_value(&meta).unwrap();
        assert_eq!(json["requestId"], "req-1");
        assert_eq!(json["durationMs"], 12);
        assert_eq!(json["reviewId"], "rev-1");
        assert_eq!(json["expertId"], "exp-1");
        assert!(json.get("request_id").is_none(), "snake_case key must not be emitted");

        // Legacy snake_case input (pre-rename logs.ndjson) still parses.
        let legacy: LogMetadata =
            serde_json::from_value(serde_json::json!({"request_id": "r", "duration_ms": 3})).unwrap();
        assert_eq!(legacy.request_id.as_deref(), Some("r"));
        assert_eq!(legacy.duration_ms, Some(3));
    }
}
