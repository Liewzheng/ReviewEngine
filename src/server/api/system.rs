//! REST API endpoints for system information: expert list, version, health status.
//!
//! @module review-engine: part of the CodeReview Board virtual engineering team
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use std::sync::Arc;

use crate::server::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/experts", get(list_experts))
        .route("/experts/{id}", put(update_expert))
        .route("/version", get(version_info))
        .route("/health", get(system_health))
}

async fn list_experts(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let cfg_opt = state.app_config.read().unwrap();
    let cfg = match cfg_opt.as_ref() {
        Some(c) => c,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "config not loaded"})),
            )
                .into_response()
        }
    };

    let experts: Vec<serde_json::Value> = cfg
        .build_expert_defs()
        .into_iter()
        .map(|e| {
            let name = &e.name;
            let id = slugify(name);
            let category = derive_category(name, &e.config.role);
            let icon = icon_for_category(&category);
            serde_json::json!({
                "id": id,
                "name": if e.config.title.is_empty() { name } else { &e.config.title },
                "category": category,
                "icon": icon,
                "enabled": e.config.enabled,
                "weight": 80,
                "description": e.config.role,
                "promptPreview": e.prompt.clone(),
                "lastReviews": [],
            })
        })
        .collect();

    Json(serde_json::json!({ "experts": experts })).into_response()
}

async fn version_info() -> Json<serde_json::Value> {
    let features: Vec<String> = {
        let mut f = vec!["cli".to_string()];
        if cfg!(feature = "python") {
            f.push("python".to_string());
        }
        f
    };
    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "features": features,
    }))
}

async fn system_health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut integrations = Vec::new();
    let mut llm_providers = Vec::new();

    let llm_configs = state.llm_configs.read().unwrap();

    // GitLab integration check
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

    Json(serde_json::json!({
        "integrations": integrations,
        "llmProviders": llm_providers,
        "overall": overall,
        "lastChecked": chrono::Utc::now().to_rfc3339(),
    }))
    .into_response()
}

fn slugify(name: &str) -> String {
    name.to_lowercase().replace([' ', '_'], "-").replace(".", "")
}

fn derive_category(name: &str, role: &str) -> String {
    let text = format!("{} {}", name, role).to_lowercase();
    if text.contains("security") || text.contains("vulnerab") || text.contains("auth") || text.contains("inject") {
        "security".to_string()
    } else if text.contains("performance") || text.contains("optim") || text.contains("speed") || text.contains("slow")
    {
        "performance".to_string()
    } else if text.contains("test") || text.contains("coverage") {
        "test-coverage".to_string()
    } else if text.contains("doc") || text.contains("comment") || text.contains("readme") {
        "documentation".to_string()
    } else if text.contains("depend")
        || text.contains("package")
        || text.contains("library")
        || text.contains("version")
    {
        "dependencies".to_string()
    } else if text.contains("access") || text.contains("a11y") || text.contains("wcag") {
        "accessibility".to_string()
    } else if text.contains("architect")
        || text.contains("design")
        || text.contains("pattern")
        || text.contains("structure")
    {
        "architecture".to_string()
    } else if text.contains("maintain") || text.contains("clean") || text.contains("refactor") {
        "maintainability".to_string()
    } else {
        "quality".to_string()
    }
}

fn icon_for_category(category: &str) -> String {
    match category {
        "security" => "Lock",
        "performance" => "TrendCharts",
        "quality" => "Check",
        "maintainability" => "Brush",
        "test-coverage" => "DocumentChecked",
        "documentation" => "Document",
        "dependencies" => "Connection",
        "accessibility" => "View",
        "architecture" => "Box",
        _ => "Star",
    }
    .to_string()
}

#[derive(Debug, serde::Deserialize)]
struct UpdateExpertRequest {
    enabled: Option<bool>,
    weight: Option<u8>,
}

async fn update_expert(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateExpertRequest>,
) -> impl IntoResponse {
    let mut cfg_opt = state.app_config.write().unwrap();
    let cfg = match cfg_opt.as_mut() {
        Some(arc) => Arc::make_mut(arc),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "config not loaded"})),
            )
                .into_response();
        }
    };

    let expert_name = cfg.review_experts.keys().find(|name| slugify(name) == id).cloned();

    let name = match expert_name {
        Some(n) => n,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "expert not found"})),
            )
                .into_response();
        }
    };

    if let Some(expert) = cfg.review_experts.get_mut(&name) {
        if let Some(enabled) = body.enabled {
            expert.enabled = enabled;
        }
        if let Some(weight) = body.weight {
            expert.weight = weight;
        }

        let category = derive_category(&name, &expert.role);
        let icon = icon_for_category(&category);
        let response = serde_json::json!({
            "id": id,
            "name": if expert.title.is_empty() { &name } else { &expert.title },
            "category": category,
            "icon": icon,
            "enabled": expert.enabled,
            "weight": expert.weight,
            "description": expert.role,
            "promptPreview": expert.prompt.clone().unwrap_or_default(),
            "lastReviews": [],
        });
        Json(response).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "expert not found"})),
        )
            .into_response()
    }
}
