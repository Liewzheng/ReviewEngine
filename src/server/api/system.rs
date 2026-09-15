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
    // handler writes to, with the persisted WebUI overrides already applied on
    // top (RENG-69) — so the list reflects the true configured state: real
    // weights (never a placeholder) and disabled experts included with
    // `enabled: false` (the management UI needs them to re-enable a card).
    // `build_expert_defs` is deliberately NOT used here: it filters disabled
    // and invalid experts, which is right for review execution but wrong for
    // a management listing.
    let experts: Vec<serde_json::Value> = cfg
        .review_experts
        .iter()
        .map(|(name, e)| expert_view(&slugify(name), name, e))
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

    // Clone out of the lock: the LLM rows below probe (`await`) and no std
    // guard may be held across an await point.
    let llm_configs: Vec<crate::models::LLMConfig> = state.llm_configs.read().unwrap().clone();

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

    // LLM rows come from the shared probe cache (RENG-36), exactly like
    // `GET /api/v1/llm/providers` and the dashboard's health section: a
    // provider whose key was just broken must not be reported `success` here
    // either. `latencyMs` stays 0 on this endpoint (the dashboard's rule,
    // RENG-32) — the card payload carries the probe's own timing.
    let health = state.llm_health.report(&llm_configs).await;
    for (llm, report) in llm_configs.iter().zip(health.iter()) {
        llm_providers.push(serde_json::json!({
            "service": format!("{} {}", llm.provider, llm.model),
            "type": "llm",
            "status": report.status.dashboard_str(),
            "latencyMs": 0,
            "message": report.message,
        }));
    }

    // `overall` counts only ENABLED providers (RENG-75): a disabled one is
    // deliberately off — never a `warning`/`error` vote — and with nothing
    // enabled the subsystem is not serving (`offline`, as when unconfigured).
    let enabled: Vec<&crate::server::api::llm_health::ProviderHealth> = health
        .iter()
        .filter(|h| h.status != crate::server::api::llm_health::ProviderStatus::Disabled)
        .collect();
    let overall = if enabled.is_empty() {
        "offline"
    } else if enabled
        .iter()
        .all(|h| h.status == crate::server::api::llm_health::ProviderStatus::Healthy)
    {
        "success"
    } else if enabled
        .iter()
        .any(|h| h.status == crate::server::api::llm_health::ProviderStatus::Healthy)
    {
        "warning"
    } else {
        "error"
    };

    // Top-level gate flag for the frontend: true iff at least one effective
    // LLM config is usable — ENABLED (RENG-75) and with a non-empty
    // `api_base` (`api_key` may stay empty for local providers). Mirrors the
    // enqueue-time gate on POST /api/v1/reviews.
    let llm_configured = llm_configs.iter().any(|c| !c.disabled && !c.api_base.trim().is_empty());

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

/// The JSON view of one configured expert — the shape `GET /system/experts`
/// and `PUT /system/experts/{id}` both return. `id` is the slug the UI uses
/// to address the expert, `name` its key in `[review_experts]`.
fn expert_view(id: &str, name: &str, expert: &crate::models::ExpertTomlDef) -> serde_json::Value {
    let category = derive_category(name, &expert.role);
    let icon = icon_for_category(&category);
    serde_json::json!({
        "id": id,
        "name": if expert.title.is_empty() { name } else { &expert.title },
        "category": category,
        "icon": icon,
        "enabled": expert.enabled,
        "weight": expert.weight,
        "description": expert.role,
        "promptPreview": expert.prompt.clone().unwrap_or_default(),
        "lastReviews": [],
    })
}

/// `PUT /api/v1/system/experts/{id}` — enable/disable an expert or change its
/// weight.
///
/// The edit is persisted AND applied to the running config (RENG-69 —
/// pre-0.10.24 it was memory-only, so every container recreate silently
/// reverted it to the config file's value):
///
/// - **DB attached** → the whole override map is upserted into `app_settings`
///   (key `experts`, see [`super::config::persist::save_expert_overrides`]) and
///   the response carries `"persisted": true`.
/// - **No DB** (`REVIEW_DISABLE_DB=1`, tests, embedded use) → memory-only, the
///   pre-0.10.24 behaviour, reported honestly as `"persisted": false` plus a
///   warning log; the response never implies the edit survives a restart.
///
/// **Ordering: persist, then apply** (RENG-69 review). The store write is
/// awaited BEFORE the runtime is touched, so a `500` means the request changed
/// nothing anywhere — the API's answer, `GET /system/experts`, the running
/// `app_config` and the database all still hold the previous value. The
/// alternative (`PUT /config`'s apply-then-persist-with-rollback) would need
/// the previous map kept aside and re-applied on failure; persist-then-apply
/// needs no rollback at all. Latency is unchanged: the handler already awaited
/// the write before answering, so only the point at which the in-memory
/// mutation happens moved. With no DB there is nothing to await, so that path
/// moves straight to the apply (and reports `persisted: false`).
///
/// **Weight validation** (RENG-69 review): a weight above
/// [`crate::config::MAX_EXPERT_WEIGHT`] is rejected with `422` — the same
/// status `PUT /config` uses for its invalid values (unsupported platform
/// type, non-http base URL), and a value the config-file schema
/// (`ExpertTomlDef::weight` 0–100, enabled weights summing to 100) can never
/// express. Rejected rather than clamped: the UI slider cannot produce one, so
/// the only callers that can are hand-written clients, and silently storing a
/// different weight than requested would make the API's answer a lie. The
/// message reaches the user — the API client renders the JSON `error` field
/// into the notification.
///
/// The applied override is published on [`AppState::expert_overrides`] so every
/// review dispatch re-applies it: neither `run_review` (REST) nor
/// `run_review_common` (webhooks) reads `app_config` — both re-resolve the
/// config file — so the override is threaded to them instead of living only in
/// memory.
async fn update_expert(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateExpertRequest>,
) -> impl IntoResponse {
    // Validate before touching anything: an out-of-range weight is refused
    // outright (see the doc comment above).
    if let Some(weight) = body.weight {
        if weight > crate::config::MAX_EXPERT_WEIGHT {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": format!(
                        "invalid weight {weight}: an expert's weight must be between 0 and {}",
                        crate::config::MAX_EXPERT_WEIGHT
                    )
                })),
            )
                .into_response();
        }
    }

    // Locate the expert (read snapshot, never a guard held across the
    // persistence await below).
    let name = {
        let cfg_opt = state.app_config.read().unwrap();
        let cfg = match cfg_opt.as_ref() {
            Some(c) => c,
            None => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({"error": "config not loaded"})),
                )
                    .into_response();
            }
        };
        match cfg.review_experts.keys().find(|name| slugify(name) == id).cloned() {
            Some(n) => n,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error": "expert not found"})),
                )
                    .into_response();
            }
        }
    };

    // Merge this request into the persisted override map (fields the body
    // omitted keep their stored value). Nothing is applied yet.
    let patch = crate::config::ExpertOverride {
        enabled: body.enabled,
        weight: body.weight,
    };
    let mut overrides = (*state.expert_overrides_snapshot()).clone();
    overrides.record(&name, patch);

    // Persist FIRST. A failure here returns before the runtime is mutated, so
    // there is no rollback to get wrong and no window in which the API's answer
    // and the process state disagree.
    let persisted = match state.db.as_ref() {
        Some(db) => {
            if overrides.is_empty() {
                // Nothing was ever overridden (an all-optional body after the
                // previous entry was emptied): the volatile state equals the
                // durable state, so a restart cannot lose anything and there is
                // no row to write.
                true
            } else {
                match super::config::persist::save_expert_overrides(db, &overrides).await {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::error!(
                            expert = %name,
                            error = %format!("{e:#}"),
                            "failed to persist expert override to the database; \
                             the running configuration is unchanged"
                        );
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({
                                "error": format!(
                                    "failed to persist the expert change to the database: {e}"
                                )
                            })),
                        )
                            .into_response();
                    }
                }
            }
        }
        None => {
            tracing::warn!(
                expert = %name,
                "expert override applied in memory only: no database is attached \
                 (REVIEW_DISABLE_DB=1 or embedded use) — it will be lost on restart"
            );
            false
        }
    };

    // Durable (or deliberately volatile): apply + publish, so the running
    // `app_config`, `GET /system/experts` and every subsequent review dispatch
    // see the edit immediately.
    state.set_expert_overrides(overrides);

    // Echo the effective expert back (read after the apply, so the response is
    // the value the server will use, not the request echo).
    let cfg_opt = state.app_config.read().unwrap();
    let mut response = match cfg_opt.as_ref().and_then(|cfg| cfg.review_experts.get(&name)) {
        Some(expert) => expert_view(&id, &name, expert),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "expert not found"})),
            )
                .into_response();
        }
    };
    drop(cfg_opt);
    if let Some(obj) = response.as_object_mut() {
        obj.insert("persisted".to_string(), serde_json::Value::Bool(persisted));
    }
    Json(response).into_response()
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
    /// remain visible in the management listing. `db` attaches a persistence
    /// store (RENG-69); `None` is the no-database deployment.
    fn expert_state_with_db(db: Option<Arc<crate::store::SqlxStore>>) -> Arc<AppState> {
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
        let mut state = AppState::new(vec![]);
        *state.app_config.write().unwrap() = Some(Arc::new(config));
        state.db = db;
        Arc::new(state)
    }

    fn state_with_experts() -> Arc<AppState> {
        expert_state_with_db(None)
    }

    /// An in-memory store with the schema applied — the same store the server
    /// would boot with.
    async fn fresh_store() -> Arc<crate::store::SqlxStore> {
        let store = crate::store::SqlxStore::new_in_memory().await.unwrap();
        store.migrate().await.unwrap();
        Arc::new(store)
    }

    /// The effective expert table of a state, as the review paths see it.
    fn effective_experts(state: &Arc<AppState>) -> HashMap<String, ExpertTomlDef> {
        state
            .app_config
            .read()
            .unwrap()
            .as_ref()
            .expect("app_config seeded")
            .review_experts
            .clone()
    }

    async fn put_expert(
        state: &Arc<AppState>,
        id: &str,
        enabled: Option<bool>,
        weight: Option<u8>,
    ) -> axum::response::Response {
        update_expert(
            State(state.clone()),
            Path(id.to_string()),
            Json(UpdateExpertRequest { enabled, weight }),
        )
        .await
        .into_response()
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

    // ─── RENG-69: expert edits are persisted, not memory-only ───

    /// (a) A UI-style PUT writes the override into the store (`app_settings`
    /// key `experts`) and applies it to the running config immediately.
    #[tokio::test]
    async fn update_expert_persists_the_override() {
        let db = fresh_store().await;
        let state = expert_state_with_db(Some(db.clone()));

        let resp = put_expert(&state, "security", Some(false), Some(45)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["enabled"], false, "the response echoes the new state: {body}");
        assert_eq!(body["weight"], 45);
        assert_eq!(
            body["persisted"], true,
            "a successful store write must be reported: {body}"
        );

        // The durable form is the `app_settings` row, keyed by expert NAME.
        let stored = crate::server::api::config::persist::load_expert_overrides(&db)
            .await
            .expect("stored overrides must load");
        assert_eq!(stored.len(), 1, "one override stored: {stored:?}");
        assert_eq!(stored.get("Security").and_then(|o| o.enabled), Some(false));
        assert_eq!(stored.get("Security").and_then(|o| o.weight), Some(45));

        // Immediately effective in the running config, and on the published
        // snapshot the review paths take.
        let effective = effective_experts(&state);
        assert!(!effective["Security"].enabled);
        assert_eq!(effective["Security"].weight, 45);
        assert_eq!(
            state.expert_overrides_snapshot().get("Security").and_then(|o| o.weight),
            Some(45)
        );

        // A second edit for another expert accumulates instead of replacing.
        assert_eq!(
            put_expert(&state, "docs", None, Some(15)).await.status(),
            StatusCode::OK
        );
        let stored = crate::server::api::config::persist::load_expert_overrides(&db)
            .await
            .unwrap();
        assert_eq!(stored.len(), 2, "both edits are stored: {stored:?}");
        assert_eq!(stored.get("Security").and_then(|o| o.enabled), Some(false));
        assert_eq!(stored.get("Docs").and_then(|o| o.weight), Some(15));
    }

    /// (b) A restart (fresh state from the config file + the stored overrides)
    /// keeps the edited value while unedited experts keep the file values, and
    /// the review-side expert set follows the override.
    #[tokio::test]
    async fn restart_replays_overrides_over_the_file_values() {
        let db = fresh_store().await;
        let state = expert_state_with_db(Some(db.clone()));
        assert_eq!(
            put_expert(&state, "security", Some(false), None).await.status(),
            StatusCode::OK
        );

        // "Restart": a brand-new state seeded from the same config file, with
        // no overrides loaded yet — exactly what `resolve_config` produces at
        // startup.
        let restarted = expert_state_with_db(Some(db.clone()));
        assert!(
            effective_experts(&restarted)["Security"].enabled,
            "the file value is the base before the replay"
        );

        let applied = crate::server::api::config::persist::load_and_apply_expert_overrides(&restarted, &db).await;
        assert_eq!(applied, 1, "exactly the edited expert is patched");

        let effective = effective_experts(&restarted);
        assert!(!effective["Security"].enabled, "the edit survived the restart");
        assert_eq!(
            effective["Security"].weight, 30,
            "the untouched field keeps the file value"
        );
        assert!(effective["Lead"].enabled, "an unedited expert keeps the file value");
        assert_eq!(effective["Lead"].weight, 50);

        // What a review would run: the disabled expert drops out of the expert
        // set, the rest keep their file weights.
        let mut resolved = restarted
            .app_config
            .read()
            .unwrap()
            .as_ref()
            .expect("seeded")
            .as_ref()
            .clone();
        restarted.expert_overrides_snapshot().apply_to(&mut resolved);
        let names: Vec<String> = resolved.build_expert_defs().into_iter().map(|e| e.name).collect();
        assert!(
            !names.contains(&"Security".to_string()),
            "a webhook/REST review must not run the disabled expert: {names:?}"
        );
        assert_eq!(names.len(), 3, "Lead + Performance + Docs: {names:?}");
    }

    /// (c) A failing store write is answered with 500, never reports success,
    /// AND leaves the process state untouched — the runtime config, the
    /// published override snapshot and the endpoint's own listing all still
    /// hold the previous value, so the answer and the state agree.
    #[tokio::test]
    async fn update_expert_surfaces_a_failed_store_write() {
        let db = fresh_store().await;
        let state = expert_state_with_db(Some(db.clone()));
        // A first, successful edit: the failed one below must leave THIS value
        // in place, not the file's.
        assert_eq!(
            put_expert(&state, "security", Some(false), None).await.status(),
            StatusCode::OK
        );
        // Break the store's schema out from under it: every subsequent write
        // fails, exactly like an unreachable database.
        ::sqlx::query("DROP TABLE app_settings")
            .execute(db.pool())
            .await
            .expect("dropping the settings table must succeed");

        let resp = put_expert(&state, "security", None, Some(99)).await;
        assert_eq!(
            resp.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "a failed persist must be an error response"
        );
        let body = body_json(resp).await;
        let error = body["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("failed to persist"),
            "the error must name the persistence failure: {body}"
        );
        assert_ne!(
            body["persisted"], true,
            "a failed write must not report success: {body}"
        );

        // Persist-then-apply: nothing was applied, so the previous edit (and
        // the file's weight) are what the server now runs.
        let effective = effective_experts(&state);
        assert!(!effective["Security"].enabled, "the earlier successful edit stands");
        assert_eq!(
            effective["Security"].weight, 30,
            "the weight of the failed request must not have been applied: {body}"
        );
        assert_eq!(
            state.expert_overrides_snapshot().get("Security").and_then(|o| o.weight),
            None,
            "the published snapshot must not carry the failed request either"
        );
        let listed = experts_body(state).await;
        assert_eq!(expert_by_id(&listed, "security")["weight"], 30, "{listed}");
    }

    /// A weight outside the schema's 0–100 range is rejected at the boundary
    /// with 422 (the status `PUT /config` uses for its invalid values) and
    /// nothing is stored or applied.
    #[tokio::test]
    async fn update_expert_rejects_an_out_of_range_weight() {
        let db = fresh_store().await;
        let state = expert_state_with_db(Some(db.clone()));

        for weight in [101u16, 200, 255] {
            let resp = put_expert(&state, "security", None, Some(weight as u8)).await;
            assert_eq!(
                resp.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "weight {weight} must be refused"
            );
            let body = body_json(resp).await;
            let error = body["error"].as_str().unwrap_or_default();
            assert!(
                error.contains("between 0 and 100"),
                "the error must state the accepted range: {body}"
            );
        }

        // Nothing was stored and nothing was applied.
        let stored = crate::server::api::config::persist::load_expert_overrides(&db)
            .await
            .unwrap();
        assert!(stored.is_empty(), "a rejected weight must not be stored: {stored:?}");
        assert!(state.expert_overrides_snapshot().is_empty());
        assert_eq!(
            effective_experts(&state)["Security"].weight,
            30,
            "the file value stands"
        );

        // The boundary itself is accepted.
        let resp = put_expert(&state, "security", None, Some(100)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["weight"], 100);
        assert_eq!(effective_experts(&state)["Security"].weight, 100);
    }

    /// A body with no field (`{}`) expresses no change: it must not invent an
    /// empty override entry, and it reports success because there is nothing
    /// that could be lost.
    #[tokio::test]
    async fn update_expert_with_an_empty_patch_stores_nothing() {
        let db = fresh_store().await;
        let state = expert_state_with_db(Some(db.clone()));

        let resp = put_expert(&state, "security", None, None).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["persisted"], true, "nothing to lose: {body}");
        assert_eq!(body["weight"], 30, "the expert is echoed unchanged: {body}");

        let stored = crate::server::api::config::persist::load_expert_overrides(&db)
            .await
            .unwrap();
        assert!(stored.is_empty(), "an empty patch must not store an entry: {stored:?}");
        assert!(state.expert_overrides_snapshot().is_empty());

        // …and after a real edit, an empty patch keeps the stored override
        // instead of wiping it (the fields it omits are untouched).
        assert_eq!(
            put_expert(&state, "security", Some(false), None).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            put_expert(&state, "security", None, None).await.status(),
            StatusCode::OK
        );
        let stored = crate::server::api::config::persist::load_expert_overrides(&db)
            .await
            .unwrap();
        assert_eq!(stored.get("Security").and_then(|o| o.enabled), Some(false));
        assert!(!effective_experts(&state)["Security"].enabled);
    }

    /// (d) Without a store (REVIEW_DISABLE_DB=1, embedded use) the endpoint
    /// keeps the pre-0.10.24 behaviour: applied in memory, but the response
    /// says honestly that it is not persisted.
    #[tokio::test]
    async fn update_expert_without_a_store_is_memory_only_and_says_so() {
        let state = expert_state_with_db(None);

        let resp = put_expert(&state, "security", Some(false), None).await;
        assert_eq!(resp.status(), StatusCode::OK, "no-store mode still applies the change");
        let body = body_json(resp).await;
        assert_eq!(body["enabled"], false);
        assert_eq!(
            body["persisted"], false,
            "without a store the change is memory-only and must say so: {body}"
        );
        assert!(!effective_experts(&state)["Security"].enabled, "still effective now");

        // The GET listing agrees with the PUT.
        let listed = experts_body(state).await;
        assert_eq!(expert_by_id(&listed, "security")["enabled"], false);
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
            disabled: false,
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

    /// RENG-36: the LLM rows report the shared probe cache's verdict, never key
    /// presence — a provider whose key was just broken must not read `success`
    /// on this endpoint either (it is what the LLM page's not-configured banner
    /// is built on, and it lists the same providers as the other two pages).
    #[tokio::test]
    async fn system_health_llm_rows_follow_probe_verdicts() {
        let mut state = AppState::new(vec![crate::models::LLMConfig {
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            api_key: "sk-broken".to_string(),
            api_base: "https://api.openai.example/v1".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            disable_thinking: None,
            disabled: false,
        }]);
        state.llm_health = Arc::new(crate::server::api::llm_health::LlmHealthStore::with_probe(
            Arc::new(|_cfg| Box::pin(async { Err("HTTP 401 Unauthorized".to_string()) })),
            std::time::Duration::from_secs(60),
        ));

        let body = health_json(state).await;
        let row = &body["llmProviders"][0];
        assert_eq!(row["service"], "openai gpt-4o");
        assert_eq!(
            row["status"], "error",
            "a stored key whose probe fails must not read success: {body}"
        );
        assert_eq!(row["message"], "HTTP 401 Unauthorized");
        assert_eq!(body["overall"], "error");
        // The gate flag is unchanged by this (config presence, not health).
        assert_eq!(body["llmConfigured"], true);
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
