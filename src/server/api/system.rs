//! REST API endpoints for system information: expert list, version, health status.
//!
//! @module review-engine: part of the CodeReview Board virtual engineering team
use axum::{
    extract::{rejection::JsonRejection, Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use std::sync::Arc;

use crate::server::auth::AuthConfig;
use crate::server::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/experts", get(list_experts))
        .route("/experts/{id}", put(update_expert))
        .route("/version", get(version_info))
        .route("/health", get(system_health))
        .route("/token", put(put_token))
        .route("/auth-status", get(auth_status))
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
        // No build.rs / version-injection mechanism exists in this repo yet;
        // fall back to common CI env vars at compile time, else "unknown".
        "commit": option_env!("GIT_COMMIT")
            .or_else(|| option_env!("GITHUB_SHA"))
            .unwrap_or("unknown"),
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

/// Body for `PUT /api/v1/system/token`.
#[derive(Debug, serde::Deserialize)]
struct PutTokenRequest {
    token: String,
}

/// Set or rotate the API auth token: persists its digest to the auth file and
/// hot-swaps the running [`AuthConfig`] so the new token takes effect
/// immediately (no restart).
///
/// Auth contract (enforced by `auth_middleware`, not re-checked here):
/// - A token is already configured → the caller must authenticate with the
///   current (old) token, the one-time bootstrap key (`X-Bootstrap-Key`), or
///   the explicit env/CLI token (`REVIEW_API_TOKEN` / `--api-token`); the
///   latter two are the self-rescue path when the current token is invalid or
///   lost. Otherwise 401.
/// - No token yet (first-run bootstrap) → reachable from a loopback bind, or
///   with the one-time bootstrap key (`X-Bootstrap-Key`) on a non-loopback
///   bind.
async fn put_token(
    Extension(auth): Extension<Arc<AuthConfig>>,
    body: Result<Json<PutTokenRequest>, JsonRejection>,
) -> impl IntoResponse {
    let Json(body) = match body {
        Ok(json) => json,
        Err(rejection) => {
            // Malformed/missing body: keep the 422 status but return the same
            // JSON error shape as every other endpoint.
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({ "error": rejection.body_text() })),
            )
                .into_response();
        }
    };
    if body.token.trim().is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "token must not be empty" })),
        )
            .into_response();
    }
    match auth.update_token(&body.token) {
        Ok(()) => Json(serde_json::json!({ "status": "saved", "configured": true })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("failed to persist API token: {e}") })),
        )
            .into_response(),
    }
}

/// Report whether an API token is configured, for the frontend's first-run
/// bootstrap detection. Deliberately unauthenticated: reveals only a boolean
/// (the token itself never leaves the server, and GET never returns it).
async fn auth_status(Extension(auth): Extension<Arc<AuthConfig>>) -> impl IntoResponse {
    let configured = auth.is_enabled();
    Json(serde_json::json!({
        "configured": configured,
        "bootstrap": !configured,
        "bootstrapKeyRequired": !configured && auth.bootstrap_key_required(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit 8: `/system/version` always exposes a `commit` string (from a
    /// compile-time env var, falling back to "unknown" when none is set).
    #[tokio::test]
    async fn version_info_includes_commit_field() {
        let json = version_info().await;
        let commit = json.0["commit"].as_str().expect("commit must be a string");
        assert!(!commit.is_empty());
        let version = json.0["version"].as_str().expect("version must be a string");
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
    }

    /// An `AuthConfig` in first-run bootstrap mode on a loopback bind, with a
    /// temp-dir auth file so `update_token` persists to an isolated location.
    fn bootstrap_auth() -> (Arc<AuthConfig>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("auth.toml");
        let auth = Arc::new(AuthConfig::resolve(None, "127.0.0.1", Some(store), None).unwrap());
        (auth, dir)
    }

    async fn put_token_response(auth: &Arc<AuthConfig>, token: &str) -> axum::response::Response {
        put_token(
            Extension(auth.clone()),
            Ok(Json(PutTokenRequest {
                token: token.to_string(),
            })),
        )
        .await
        .into_response()
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn put_token_bootstrap_sets_and_persists_digest() {
        let (auth, dir) = bootstrap_auth();
        let resp = put_token_response(&auth, "my-ui-token").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(auth.is_enabled(), "token must take effect immediately");
        assert_eq!(
            body_json(resp).await,
            serde_json::json!({"status": "saved", "configured": true})
        );

        // Persisted, and never as plaintext.
        let content = std::fs::read_to_string(dir.path().join("auth.toml")).unwrap();
        assert!(
            !content.contains("my-ui-token"),
            "auth file must not store the raw token"
        );
        assert!(content.contains("api_token_sha256"));
    }

    #[tokio::test]
    async fn put_token_empty_rejected_422() {
        let (auth, _dir) = bootstrap_auth();
        let resp = put_token_response(&auth, "   ").await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!auth.is_enabled());
    }

    #[tokio::test]
    async fn put_token_rotates_configured_token() {
        let (auth, dir) = bootstrap_auth();
        auth.update_token("first-token").unwrap();
        let resp = put_token_response(&auth, "second-token").await;
        assert_eq!(resp.status(), StatusCode::OK);

        let req = |tok: &str| {
            axum::http::Request::builder()
                .uri("/system/version")
                .header("Authorization", format!("Bearer {tok}"))
                .body(axum::body::Body::empty())
                .unwrap()
        };
        assert!(auth.check(&req("second-token")), "new token must be effective");
        assert!(!auth.check(&req("first-token")), "old token must stop working");
        let content = std::fs::read_to_string(dir.path().join("auth.toml")).unwrap();
        assert!(!content.contains("second-token"));
    }

    #[tokio::test]
    async fn auth_status_reflects_bootstrap_then_configured() {
        let (auth, _dir) = bootstrap_auth();

        let resp = auth_status(Extension(auth.clone())).await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["configured"], false);
        assert_eq!(body["bootstrap"], true);
        assert_eq!(body["bootstrapKeyRequired"], false); // loopback bind

        auth.update_token("t").unwrap();
        let resp = auth_status(Extension(auth)).await.into_response();
        let body = body_json(resp).await;
        assert_eq!(body["configured"], true);
        assert_eq!(body["bootstrap"], false);
        assert_eq!(body["bootstrapKeyRequired"], false);
    }
}
