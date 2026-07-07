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

    let (items, _total) = store.list(None, 1, 1000).await;

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
    let mut recent: Vec<&TaskEntry> = items
        .iter()
        .filter(|e| e.state == TaskState::Completed || e.state == TaskState::Failed)
        .collect();
    recent.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    recent.truncate(10);

    recent
        .iter()
        .map(|e| {
            let meta = &e.source_meta;
            serde_json::json!({
                "id": e.task_id.to_string(),
                "mrTitle": meta.mr_title.as_deref().unwrap_or("Untitled Review"),
                "project": meta.project.as_deref().unwrap_or("unknown"),
                "author": {
                    "name": meta.author_name.as_deref().unwrap_or("unknown"),
                    "avatarUrl": meta.author_avatar_url,
                },
                "status": match e.state {
                    TaskState::Completed => "success",
                    TaskState::Failed => "failed",
                    _ => "running",
                },
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
