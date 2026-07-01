use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;
use std::sync::Arc;

use crate::server::AppState;

pub async fn get_progress(
    State(state): State<Arc<AppState>>,
    Path(review_id): Path<String>,
) -> impl axum::response::IntoResponse {
    match state.progress_map {
        Some(ref map) => {
            let Ok(map) = map.read() else {
                return (StatusCode::INTERNAL_SERVER_ERROR, "lock poisoned").into_response();
            };
            match map.get(&review_id) {
                Some(p) => Json(p.clone()).into_response(),
                None => (StatusCode::NOT_FOUND, Json(json!({"error": "not found", "code": 404}))).into_response(),
            }
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "progress not enabled", "code": 404})),
        )
            .into_response(),
    }
}

pub async fn list_progress(State(state): State<Arc<AppState>>) -> impl axum::response::IntoResponse {
    match state.progress_map {
        Some(ref map) => {
            let Ok(map) = map.read() else {
                return (StatusCode::INTERNAL_SERVER_ERROR, "lock poisoned").into_response();
            };
            let entries: Vec<crate::progress::ReviewProgress> = map.values().cloned().collect();
            Json(entries).into_response()
        }
        None => Json(Vec::<crate::progress::ReviewProgress>::new()).into_response(),
    }
}
