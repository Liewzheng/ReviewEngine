//! Server-Sent Events (SSE) endpoint for real-time task status updates.
//!
//! @module review-engine: part of the CodeReview Board virtual engineering team
use axum::{
    extract::State,
    response::sse::{Event, Sse},
    routing::get,
    Router,
};
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::server::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/", get(event_stream))
}

async fn event_stream(
    State(state): State<Arc<AppState>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.task_store.as_ref().map(|s| s.subscribe()).unwrap_or_else(|| {
        let (tx, _) = tokio::sync::broadcast::channel(1);
        tx.subscribe()
    });

    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => {
            let mut data = serde_json::json!({
                "task_id": event.task_id,
                "status": event.status,
                "event": event.event,
            });
            if let Some(v) = event.mr_title {
                data["mr_title"] = serde_json::json!(v);
            }
            if let Some(v) = event.project {
                data["project"] = serde_json::json!(v);
            }
            if let Some(v) = event.progress {
                data["progress"] = serde_json::json!(v);
            }
            if let Some(v) = event.expert_name {
                data["expert_name"] = serde_json::json!(v);
            }
            if let Some(v) = event.elapsed_ms {
                data["elapsed_ms"] = serde_json::json!(v);
            }
            Some(Ok::<_, Infallible>(Event::default().data(data.to_string())))
        }
        Err(_) => None,
    });

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keep-alive"),
    )
}
