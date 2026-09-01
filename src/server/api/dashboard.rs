//! REST API endpoints for the dashboard overview page.
//!
//! Aggregates KPIs, 24h trend, system health, and recent reviews
//! from the task store and other runtime state.

use axum::{extract::State, response::IntoResponse, routing::get, Json, Router};
use std::sync::Arc;

use crate::server::task_queue::{TaskEntry, TaskState};
use crate::server::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/", get(get_dashboard))
}

async fn get_dashboard(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let store = match &state.task_store {
        Some(s) => s.clone(),
        None => {
            return Json(serde_json::json!({
                "kpis": default_kpis(),
                "trend": default_trend(),
                "health": default_health(),
                "recentReviews": [],
            }))
            .into_response()
        }
    };

    let (items, _total) = store.list(None, 1, 1000, None, None, None, None, None).await;

    let kpis = compute_kpis(&items);
    let trend = compute_trend(&items);
    let health = compute_health(&state).await;
    let recent_reviews = compute_recent_reviews(&items);

    Json(serde_json::json!({
        "kpis": kpis,
        "trend": trend,
        "health": health,
        "recentReviews": recent_reviews,
    }))
    .into_response()
}

fn compute_kpis(items: &[TaskEntry]) -> serde_json::Value {
    let _total = items.len() as u64;
    let completed = items.iter().filter(|e| e.state == TaskState::Completed).count() as u64;
    let failed = items.iter().filter(|e| e.state == TaskState::Failed).count() as u64;
    let active = items.iter().filter(|e| e.state == TaskState::Running).count() as u64;
    let pending = items.iter().filter(|e| e.state == TaskState::Pending).count() as u64;

    let success_rate = if completed + failed > 0 {
        completed as f64 * 100.0 / (completed + failed) as f64
    } else {
        100.0
    };

    let avg_duration: u64 = items
        .iter()
        .filter(|e| e.state == TaskState::Completed)
        .filter_map(|e| e.duration_ms())
        .reduce(|a, b| a + b)
        .and_then(|total| total.checked_div(completed))
        .unwrap_or_default();

    serde_json::json!({
        "reviewsThisWeek": completed,
        "reviewsTrend": 0.0,
        "activeQueue": active + pending,
        "successRate": (success_rate * 10.0).round() / 10.0,
        "successTrend": 0.0,
        "avgDurationMs": avg_duration,
        "durationTrend": 0.0,
    })
}

fn compute_trend(items: &[TaskEntry]) -> Vec<serde_json::Value> {
    let now = chrono::Utc::now();
    let mut points = Vec::new();
    for i in (0..24).rev() {
        let hour_start = now - chrono::Duration::hours(i + 1);
        let hour_end = now - chrono::Duration::hours(i);
        let count = items
            .iter()
            .filter(|e| e.created_at >= hour_start && e.created_at < hour_end)
            .count() as u64;
        points.push(serde_json::json!({
            "time": hour_end.timestamp(),
            "value": count,
        }));
    }
    points
}

async fn compute_health(state: &AppState) -> serde_json::Value {
    let mut integrations = Vec::new();

    let llm_configs = state.llm_configs.read().unwrap();

    // GitLab integration check (presence of token implies configured)
    let gitlab_configured = llm_configs
        .iter()
        .any(|c| c.provider.to_lowercase().contains("gitlab") || c.api_base.to_lowercase().contains("gitlab"));
    integrations.push(serde_json::json!({
        "service": "GitLab API",
        "type": "integration",
        "status": if gitlab_configured { "success" } else { "offline" },
        "latencyMs": 0,
        "message": if gitlab_configured { "Configured" } else { "Not configured" },
    }));

    // GitHub integration check
    let github_configured = llm_configs
        .iter()
        .any(|c| c.provider.to_lowercase().contains("github") || c.api_base.to_lowercase().contains("github"));
    integrations.push(serde_json::json!({
        "service": "GitHub API",
        "type": "integration",
        "status": if github_configured { "success" } else { "offline" },
        "latencyMs": 0,
        "message": if github_configured { "Configured" } else { "Not configured" },
    }));

    let mut llm_providers = Vec::new();
    for llm in llm_configs.iter() {
        let has_key = !llm.api_key.is_empty();
        llm_providers.push(serde_json::json!({
            "service": format!("{} {}", llm.provider, llm.model),
            "type": "llm",
            "status": if has_key { "success" } else { "offline" },
            "latencyMs": 0,
            "message": if has_key { "Configured" } else { "Missing API key" },
        }));
    }

    let overall = if llm_providers.is_empty() { "offline" } else { "success" };

    serde_json::json!({
        "integrations": integrations,
        "llmProviders": llm_providers,
        "overall": overall,
        "lastChecked": chrono::Utc::now().to_rfc3339(),
    })
}

fn compute_recent_reviews(items: &[TaskEntry]) -> Vec<serde_json::Value> {
    // No state filter: all tasks (including pending/cancelled) surface here,
    // each with its real status vocabulary.
    let mut recent: Vec<&TaskEntry> = items.iter().collect();
    recent.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    recent.truncate(10);

    recent
        .iter()
        .map(|e| {
            let meta = &e.source_meta;
            serde_json::json!({
                "id": e.task_id.to_string(),
                // Absent values are `null`, consistent with `/reviews` (the
                // frontend applies its own display defaults).
                "mrTitle": meta.mr_title.clone(),
                "project": meta.project.clone(),
                "author": {
                    "name": meta.author_name.clone(),
                    "avatarUrl": meta.author_avatar_url.clone(),
                },
                // Real task state vocabulary, consistent with `/reviews`.
                "status": super::review::task_status_str(&e.state),
                "durationMs": e.duration_ms().unwrap_or(0),
                "createdAt": e.created_at.to_rfc3339(),
            })
        })
        .collect()
}

fn default_kpis() -> serde_json::Value {
    serde_json::json!({
        "reviewsThisWeek": 0,
        "reviewsTrend": 0.0,
        "activeQueue": 0,
        "successRate": 100.0,
        "successTrend": 0.0,
        "avgDurationMs": 0,
        "durationTrend": 0.0,
    })
}

fn default_trend() -> Vec<serde_json::Value> {
    let now = chrono::Utc::now();
    (0..24)
        .rev()
        .map(|i| {
            serde_json::json!({
                "time": (now - chrono::Duration::hours(i)).timestamp(),
                "value": 0,
            })
        })
        .collect()
}

fn default_health() -> serde_json::Value {
    serde_json::json!({
        "integrations": [],
        "llmProviders": [],
        "overall": "offline",
        "lastChecked": chrono::Utc::now().to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::task_queue::SourceMeta;

    fn entry(id: &str, state: TaskState) -> TaskEntry {
        TaskEntry {
            task_id: uuid::Uuid::parse_str(id).unwrap(),
            state,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            result: None,
            error: None,
            request: None,
            source_meta: SourceMeta {
                mr_title: Some("MR".to_string()),
                ..SourceMeta::default()
            },
            progress: None,
            expert_name: None,
        }
    }

    /// Unit 5: `recentReviews` reports the real task state vocabulary
    /// (pending/running/completed/failed/cancelled), consistent with `/reviews`,
    /// and surfaces every state instead of only completed/failed.
    #[test]
    fn recent_reviews_use_real_status_vocabulary() {
        let items = vec![
            entry("00000000-0000-0000-0000-000000000001", TaskState::Pending),
            entry("00000000-0000-0000-0000-000000000002", TaskState::Running),
            entry("00000000-0000-0000-0000-000000000003", TaskState::Completed),
            entry("00000000-0000-0000-0000-000000000004", TaskState::Failed),
            entry("00000000-0000-0000-0000-000000000005", TaskState::Cancelled),
        ];
        let recent = compute_recent_reviews(&items);
        assert_eq!(recent.len(), 5, "every state must surface in recentReviews");

        let by_id: std::collections::HashMap<String, String> = recent
            .iter()
            .map(|r| {
                (
                    r["id"].as_str().unwrap().to_string(),
                    r["status"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(by_id["00000000-0000-0000-0000-000000000001"], "pending");
        assert_eq!(by_id["00000000-0000-0000-0000-000000000002"], "running");
        assert_eq!(by_id["00000000-0000-0000-0000-000000000003"], "completed");
        assert_eq!(by_id["00000000-0000-0000-0000-000000000004"], "failed");
        assert_eq!(by_id["00000000-0000-0000-0000-000000000005"], "cancelled");
    }

    /// `None` fallback: without a store the dashboard serves documented
    /// defaults (zero KPIs, empty recent reviews, offline health) instead of
    /// 503 — a pure guard, unreachable in production where the store is
    /// always present.
    #[tokio::test]
    async fn dashboard_without_store_serves_defaults() {
        let mut state = AppState::new(vec![]);
        state.task_store = None;
        let resp = get_dashboard(State(std::sync::Arc::new(state))).await.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["kpis"]["reviewsThisWeek"], 0);
        assert_eq!(json["kpis"]["activeQueue"], 0);
        assert!(json["recentReviews"].as_array().unwrap().is_empty());
        assert_eq!(json["health"]["overall"], "offline");
    }

    /// With a store the dashboard reflects the real task store — completed
    /// reviews from the webhook path surface as recentReviews (the exact
    /// defect this fix closes).
    #[tokio::test]
    async fn dashboard_with_store_shows_webhook_recorded_review() {
        let state = AppState::new(vec![]);
        let store = state.task_store.clone().unwrap();
        let id = store.create(None).await;
        store.update(id, TaskState::Completed, None, None).await;

        let resp = get_dashboard(State(std::sync::Arc::new(state))).await.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["kpis"]["reviewsThisWeek"], 1);
        let recent = json["recentReviews"].as_array().unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0]["id"], id.to_string());
        assert_eq!(recent[0]["status"], "completed");
    }
}
