//! REST API endpoints for log streaming (SSE) and download.
//!
//! Provides real-time log events via SSE and a bulk download endpoint.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Sse},
    routing::get,
    Router,
};
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::server::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(log_stream))
        .route("/download", get(download_logs))
}

async fn log_stream(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let collector = match state.log_collector.as_ref() {
        Some(c) => c.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Log collector not initialized",
            )
                .into_response();
        }
    };

    let rx = {
        let c = collector.lock().unwrap();
        c.subscribe()
    };

    let stream = BroadcastStream::new(rx).filter_map(|result| {
        match result {
            Ok(entry) => {
                let data = serde_json::to_string(&entry).ok()?;
                Some(Ok::<_, std::convert::Infallible>(
                    axum::response::sse::Event::default().data(data),
                ))
            }
            Err(_) => None,
        }
    });

    Sse::new(stream)
        .keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text(""),
        )
        .into_response()
}

async fn download_logs(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let collector = match state.log_collector.as_ref() {
        Some(c) => c.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Log collector not initialized",
            )
                .into_response();
        }
    };

    let entries = {
        let c = collector.lock().unwrap();
        c.recent_entries(1000)
    };

    let mut body = String::new();
    for entry in entries {
        if let Ok(line) = serde_json::to_string(&entry) {
            body.push_str(&line);
            body.push('\n');
        }
    }

    (
        StatusCode::OK,
        [("Content-Type", "application/x-ndjson")],
        body,
    )
        .into_response()
}
