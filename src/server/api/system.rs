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

    // Read straight from `review_experts` — the same config source the PUT
    // handler writes to — so the list reflects the true configured state:
    // real weights (never a placeholder) and disabled experts included with
    // `enabled: false` (the management UI needs them to re-enable a card).
    // `build_expert_defs` is deliberately NOT used here: it filters disabled
    // and invalid experts, which is right for review execution but wrong for
    // a management listing.
    let experts: Vec<serde_json::Value> = cfg
        .review_experts
        .iter()
        .map(|(name, e)| {
            let id = slugify(name);
            let category = derive_category(name, &e.role);
            let icon = icon_for_category(&category);
            serde_json::json!({
                "id": id,
                "name": if e.title.is_empty() { name } else { &e.title },
                "category": category,
                "icon": icon,
                "enabled": e.enabled,
                "weight": e.weight,
                "description": e.role,
                "promptPreview": e.prompt.clone().unwrap_or_default(),
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

    // Top-level gate flag for the frontend: true iff at least one effective
    // LLM config is usable — a non-empty `api_base` (`api_key` may stay
    // empty for local providers). Mirrors the enqueue-time gate on
    // POST /api/v1/reviews.
    let llm_configured = llm_configs.iter().any(|c| !c.api_base.trim().is_empty());

    // Persistence backend actually in use (0.10.0): "postgresql" / "sqlite"
    // from the store's connect-time URL discrimination; "disabled" when no
    // DB is attached (`REVIEW_DISABLE_DB=1`, tests, embedded use).
    let storage_backend = state
        .db
        .as_ref()
        .map(|db| db.backend_kind().as_str())
        .unwrap_or("disabled");

    Json(serde_json::json!({
        "integrations": integrations,
        "llmProviders": llm_providers,
        "llmConfigured": llm_configured,
        "storage_backend": storage_backend,
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
    use crate::models::{
        AppConfig, DiffConfig, ExpertTomlDef, LanguagesConfig, RateLimitConfig, ReportConfig, ScoringConfig,
    };
    use std::collections::HashMap;

    fn expert_def(title: &str, role: &str, weight: u8, enabled: bool) -> ExpertTomlDef {
        ExpertTomlDef {
            enabled,
            title: title.to_string(),
            role: role.to_string(),
            weight,
            prompt: Some(format!("{title} prompt")),
            ..Default::default()
        }
    }

    /// Expert fixture per the audit notes: four enabled experts whose
    /// weights sum to 100 (docs=5), plus one disabled expert that must
    /// remain visible in the management listing.
    fn state_with_experts() -> Arc<AppState> {
        let mut review_experts = HashMap::new();
        review_experts.insert(
            "Lead".to_string(),
            expert_def("Lead Reviewer", "Overall review lead", 50, true),
        );
        review_experts.insert(
            "Security".to_string(),
            expert_def("Security Lead", "Security vulnerabilities and injection", 30, true),
        );
        review_experts.insert(
            "Performance".to_string(),
            expert_def("Performance", "Performance optimization", 15, true),
        );
        review_experts.insert(
            "Docs".to_string(),
            expert_def("Docs", "Documentation and comments", 5, true),
        );
        review_experts.insert(
            "Experimental".to_string(),
            expert_def("Experimental", "Experimental quality checks", 0, false),
        );
        let config = AppConfig {
            project: None,
            report: ReportConfig::default(),
            review_experts,
            commands: HashMap::new(),
            scoring: ScoringConfig::default(),
            llm: Vec::new(),
            max_team_size: None,
            max_concurrent_llm_calls: None,
            output_dir: String::new(),
            diff: DiffConfig::default(),
            rate_limit: RateLimitConfig::default(),
            languages: LanguagesConfig::default(),
        };
        let state = Arc::new(AppState::new(vec![]));
        *state.app_config.write().unwrap() = Some(Arc::new(config));
        state
    }

    async fn experts_body(state: Arc<AppState>) -> serde_json::Value {
        let resp = list_experts(State(state)).await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        body_json(resp).await
    }

    fn expert_by_id<'a>(body: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
        body["experts"]
            .as_array()
            .expect("experts array")
            .iter()
            .find(|e| e["id"] == id)
            .unwrap_or_else(|| panic!("expert '{id}' missing from {body}"))
    }

    /// Regression for the live-audit finding: GET hardcoded `"weight": 80`
    /// for every expert. The listing must carry each expert's configured
    /// weight (docs=5, enabled weights summing to 100 here).
    #[tokio::test]
    async fn list_experts_returns_configured_weights_not_placeholder() {
        let body = experts_body(state_with_experts()).await;
        let experts = body["experts"].as_array().expect("experts array");
        assert_eq!(experts.len(), 5, "disabled experts must stay listed");

        assert_eq!(expert_by_id(&body, "lead")["weight"], 50);
        assert_eq!(expert_by_id(&body, "security")["weight"], 30);
        assert_eq!(expert_by_id(&body, "performance")["weight"], 15);
        assert_eq!(expert_by_id(&body, "docs")["weight"], 5);

        let enabled_weight_sum: u64 = experts
            .iter()
            .filter(|e| e["enabled"] == true)
            .map(|e| e["weight"].as_u64().unwrap())
            .sum();
        assert_eq!(enabled_weight_sum, 100);

        // Disabled experts keep their real state instead of vanishing.
        assert_eq!(expert_by_id(&body, "experimental")["enabled"], false);
        assert_eq!(expert_by_id(&body, "experimental")["weight"], 0);
    }

    /// GET must agree with PUT: the audit caught values jumping because PUT
    /// returned the true weight while GET served the hardcoded placeholder.
    #[tokio::test]
    async fn list_experts_reflects_put_updates() {
        let state = state_with_experts();
        let resp = update_expert(
            State(state.clone()),
            Path("docs".to_string()),
            Json(UpdateExpertRequest {
                enabled: None,
                weight: Some(25),
            }),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["weight"], 25);

        let body = experts_body(state).await;
        assert_eq!(expert_by_id(&body, "docs")["weight"], 25);
    }

    #[tokio::test]
    async fn list_experts_503_without_loaded_config() {
        let resp = list_experts(State(Arc::new(AppState::new(vec![]))))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

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

    fn llm_config(api_base: &str) -> crate::models::LLMConfig {
        crate::models::LLMConfig {
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            api_key: String::new(),
            api_base: api_base.to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            disable_thinking: None,
        }
    }

    async fn health_json(state: AppState) -> serde_json::Value {
        let resp = system_health(State(Arc::new(state))).await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        body_json(resp).await
    }

    /// `/system/health` exposes a top-level `llmConfigured` flag: true iff at
    /// least one effective LLM config has a non-empty `api_base` (`api_key`
    /// may stay empty — local providers need no key).
    #[tokio::test]
    async fn system_health_reports_llm_configured_flag() {
        // No configs at all → not configured.
        let body = health_json(AppState::new(vec![])).await;
        assert_eq!(body["llmConfigured"], false, "empty configs must report false: {body}");

        // Entries exist but none has an api_base (the shipped demo-env
        // failure mode) → still not configured.
        let body = health_json(AppState::new(vec![llm_config(""), llm_config("   ")])).await;
        assert_eq!(
            body["llmConfigured"], false,
            "entries without api_base must report false: {body}"
        );

        // At least one entry with a non-empty api_base → configured.
        let body = health_json(AppState::new(vec![
            llm_config(""),
            llm_config("http://localhost:11434/v1"),
        ]))
        .await;
        assert_eq!(
            body["llmConfigured"], true,
            "an entry with api_base must report true: {body}"
        );
    }

    /// `/system/health` exposes `storage_backend`: "disabled" when no DB is
    /// attached (`REVIEW_DISABLE_DB=1`, tests, embedded use).
    #[tokio::test]
    async fn system_health_reports_storage_backend_disabled_without_db() {
        let body = health_json(AppState::new(vec![])).await;
        assert_eq!(body["storage_backend"], "disabled", "no db attached: {body}");
    }

    /// With an in-memory SQLite store attached, `storage_backend` reports
    /// "sqlite". The "postgresql" value is covered function-level in
    /// `store::tests::backend_kind_discriminates_by_url_scheme` (no live PG
    /// in unit tests).
    #[tokio::test]
    async fn system_health_reports_storage_backend_sqlite_with_db() {
        let mut state = AppState::new(vec![]);
        state.db = Some(Arc::new(crate::store::SqlxStore::new_in_memory().await.unwrap()));
        let body = health_json(state).await;
        assert_eq!(body["storage_backend"], "sqlite", "sqlite store attached: {body}");
    }
}
