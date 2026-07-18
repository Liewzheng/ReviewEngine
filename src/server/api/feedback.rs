//! REST API endpoints for finding feedback (`/feedback`).
//!
//! `POST /` records a user verdict (`useful` / `false_positive`) for a
//! finding, identified either by a precomputed `finding_fingerprint` or
//! by the convenient `(file, line, title, category)` form from which the
//! server computes the fingerprint. `GET /stats` returns aggregated
//! totals, the false-positive rate, and a per-category breakdown.
//!
//! @module review-engine: part of the CodeReview Board virtual engineering team
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::server::feedback::{fingerprint, FindingFeedback, Verdict};
use crate::server::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", post(submit_feedback))
        .route("/stats", get(get_feedback_stats))
}

/// Raw request body for `POST /feedback`. Either `finding_fingerprint`
/// or the full `(file, title, category)` triple must be present;
/// `verdict` is validated manually so all malformed bodies yield 400.
#[derive(Debug, Deserialize)]
struct FeedbackRequest {
    #[serde(default)]
    finding_fingerprint: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    verdict: Option<Verdict>,
    #[serde(default)]
    comment: Option<String>,
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message.into() })),
    )
        .into_response()
}

async fn submit_feedback(State(state): State<Arc<AppState>>, Json(body): Json<serde_json::Value>) -> Response {
    let store = match &state.feedback_store {
        Some(s) => s.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "feedback store not initialized" })),
            )
                .into_response()
        }
    };

    let req: FeedbackRequest = match serde_json::from_value(body) {
        Ok(req) => req,
        Err(e) => return bad_request(format!("invalid feedback body: {e}")),
    };

    let verdict = match req.verdict {
        Some(v) => v,
        None => return bad_request("verdict is required (\"useful\" or \"false_positive\")"),
    };

    // Resolve the fingerprint: use the supplied one, or compute it from
    // the locating fields. Empty/whitespace fingerprints are rejected.
    let finding_fingerprint = match req.finding_fingerprint.filter(|fp| !fp.trim().is_empty()) {
        Some(fp) => fp,
        None => {
            let mut missing = Vec::new();
            if req.file.as_deref().map(str::trim).unwrap_or_default().is_empty() {
                missing.push("file");
            }
            if req.title.as_deref().map(str::trim).unwrap_or_default().is_empty() {
                missing.push("title");
            }
            if req.category.as_deref().map(str::trim).unwrap_or_default().is_empty() {
                missing.push("category");
            }
            if !missing.is_empty() {
                return bad_request(format!(
                    "finding_fingerprint is missing; provide it directly or supply {} (line optional)",
                    missing.join(", ")
                ));
            }
            // The empty checks above guarantee these are present.
            fingerprint(
                req.file.as_deref().unwrap_or_default(),
                req.line,
                req.title.as_deref().unwrap_or_default(),
                req.category.as_deref().unwrap_or_default(),
            )
        }
    };

    let feedback = FindingFeedback {
        finding_fingerprint,
        verdict,
        comment: req.comment.filter(|c| !c.trim().is_empty()),
        category: req.category.filter(|c| !c.trim().is_empty()),
        created_at: chrono::Utc::now(),
    };

    match store.record(feedback.clone()) {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!(feedback))).into_response(),
        Err(e) => {
            tracing::warn!("Feedback: failed to persist: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("failed to persist feedback: {e}") })),
            )
                .into_response()
        }
    }
}

async fn get_feedback_stats(State(state): State<Arc<AppState>>) -> Response {
    match &state.feedback_store {
        Some(store) => Json(serde_json::json!(store.stats())).into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "feedback store not initialized" })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::feedback::FeedbackStore;

    fn test_state(path: Option<std::path::PathBuf>) -> Arc<AppState> {
        let mut state = AppState::new(vec![]);
        state.feedback_store = Some(Arc::new(FeedbackStore::with_path(path)));
        Arc::new(state)
    }

    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_post_with_locating_fields_returns_200_and_fingerprint() {
        let state = test_state(None);
        let resp = submit_feedback(
            State(state.clone()),
            Json(serde_json::json!({
                "file": "src/main.rs",
                "line": 42,
                "title": "SQL injection",
                "category": "security",
                "verdict": "false_positive",
                "comment": "input is sanitised upstream"
            })),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let expected = fingerprint("src/main.rs", Some(42), "SQL injection", "security");
        assert_eq!(body["finding_fingerprint"], expected);
        assert_eq!(body["verdict"], "false_positive");
        assert_eq!(body["comment"], "input is sanitised upstream");
        assert_eq!(body["category"], "security");
        assert!(body["created_at"].is_string());
        assert_eq!(state.feedback_store.as_ref().unwrap().entries().len(), 1);
    }

    #[tokio::test]
    async fn test_post_with_fingerprint_only_returns_200() {
        let state = test_state(None);
        let resp = submit_feedback(
            State(state.clone()),
            Json(serde_json::json!({
                "finding_fingerprint": "0123456789abcdef",
                "verdict": "useful"
            })),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["finding_fingerprint"], "0123456789abcdef");
        assert_eq!(body["verdict"], "useful");
        // No category supplied: the field is omitted from the record.
        assert!(body.get("category").is_none());
    }

    #[tokio::test]
    async fn test_post_missing_verdict_returns_400() {
        let state = test_state(None);
        let resp = submit_feedback(
            State(state),
            Json(serde_json::json!({
                "finding_fingerprint": "0123456789abcdef"
            })),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert!(body["error"].as_str().unwrap().contains("verdict"));
    }

    #[tokio::test]
    async fn test_post_invalid_verdict_returns_400() {
        let state = test_state(None);
        let resp = submit_feedback(
            State(state),
            Json(serde_json::json!({
                "finding_fingerprint": "0123456789abcdef",
                "verdict": "meh"
            })),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_post_without_fingerprint_or_locating_fields_returns_400() {
        let state = test_state(None);
        let resp = submit_feedback(
            State(state),
            Json(serde_json::json!({
                "verdict": "useful",
                "title": "SQL injection"
            })),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        let error = body["error"].as_str().unwrap();
        assert!(error.contains("file"));
        assert!(error.contains("category"));
        assert!(!error.contains("title,"));
    }

    #[tokio::test]
    async fn test_post_without_store_returns_503() {
        let state = Arc::new(AppState::new(vec![]));
        let resp = submit_feedback(
            State(state),
            Json(serde_json::json!({
                "finding_fingerprint": "0123456789abcdef",
                "verdict": "useful"
            })),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_get_stats_returns_aggregates() {
        let state = test_state(None);
        for (fp, verdict, category) in [
            ("fp1", "useful", "security"),
            ("fp2", "false_positive", "security"),
            ("fp3", "useful", "quality"),
        ] {
            let resp = submit_feedback(
                State(state.clone()),
                Json(serde_json::json!({
                    "finding_fingerprint": fp,
                    "verdict": verdict,
                    "category": category
                })),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK);
        }

        let resp = get_feedback_stats(State(state)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["total"], 3);
        assert_eq!(body["useful"], 2);
        assert_eq!(body["false_positive"], 1);
        assert!((body["false_positive_rate"].as_f64().unwrap() - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(body["by_category"]["security"]["total"], 2);
        assert!((body["by_category"]["security"]["false_positive_rate"].as_f64().unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(body["by_category"]["quality"]["useful"], 1);
    }

    #[tokio::test]
    async fn test_get_stats_without_store_returns_503() {
        let state = Arc::new(AppState::new(vec![]));
        let resp = get_feedback_stats(State(state)).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
