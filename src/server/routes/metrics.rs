use axum::extract::State;
use prometheus::{Encoder, TextEncoder};
use std::sync::Arc;

use crate::server::AppState;

pub async fn metrics(State(state): State<Arc<AppState>>) -> (axum::http::StatusCode, String) {
    if let Some(ref registry) = state.registry {
        let encoder = TextEncoder::new();
        let metric_families = registry.gather();
        let mut output = Vec::new();
        if encoder.encode(&metric_families, &mut output).is_ok() {
            return (axum::http::StatusCode::OK, String::from_utf8_lossy(&output).to_string());
        }
    }
    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_metrics_returns_empty_without_registry() {
        let state = Arc::new(AppState::new(vec![]));
        let (status, _body) = metrics(axum::extract::State(state)).await;
        assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    }
}
