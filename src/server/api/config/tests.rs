use super::get_config;
use super::mask_secrets;
use super::put_config;
use super::types::API_KEY_MASK;
use super::*;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;

/// Seed an `AppState` with one openai provider carrying `key`, wired the
/// same way `serve` does: `app_config.llm` + `ui_config` built from it.
fn state_with_openai(key: &str) -> Arc<AppState> {
    let app: crate::models::AppConfig = serde_json::from_value(serde_json::json!({
        "llm": [{
            "provider": "openai",
            "model": "gpt-4o",
            "api_key": key,
            "api_base": "https://api.openai.com/v1",
            "max_tokens": 4096,
            "temperature": 0.7
        }]
    }))
    .expect("minimal AppConfig must deserialize");
    let state = Arc::new(AppState::new(app.llm.clone()));
    *state.app_config.write().unwrap() = Some(Arc::new(app.clone()));
    *state.ui_config.write().unwrap() = UiConfig::from_app_config(&app);
    state
}

fn stored_openai_key(state: &Arc<AppState>) -> String {
    state
        .app_config
        .read()
        .unwrap()
        .as_ref()
        .expect("app_config seeded")
        .llm
        .iter()
        .find(|c| c.provider == "openai")
        .map(|c| c.api_key.clone())
        .unwrap_or_default()
}

/// Security regression: `GET /config` must never return a live LLM key.
#[tokio::test]
async fn get_config_never_leaks_llm_api_key() {
    let state = state_with_openai("sk-super-secret");
    let resp = get_config(State(state)).await.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["llm"]["openaiApiKey"], API_KEY_MASK);
    assert_ne!(body["llm"]["openaiApiKey"], "sk-super-secret");
    let providers = body["llm"]["providers"].as_array().expect("providers array");
    assert_eq!(providers[0]["apiKey"], API_KEY_MASK);
    assert_ne!(providers[0]["apiKey"], "sk-super-secret");
    // No field anywhere in the response may carry the secret.
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("sk-super-secret"), "secret leaked in {serialized}");
}

/// A GET → PUT round trip with masked keys (`***`) must keep the real key
/// server-side — never replace it with the mask.
#[tokio::test]
async fn put_config_masked_round_trip_preserves_key() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let state = state_with_openai("sk-primary");
    let mut ui = state.ui_config.read().unwrap().clone();
    mask_secrets(&mut ui);

    let resp = put_config(
        State(state.clone()),
        Json(serde_json::to_value(&ui).expect("UiConfig must serialize")),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(stored_openai_key(&state), "sk-primary");
    // And the persisted UI config still surfaces the mask, not the secret.
    assert_eq!(state.ui_config.read().unwrap().llm.openai_api_key, API_KEY_MASK);
}

/// "Leave blank = unchanged": a PUT with an empty key keeps the stored key.
#[tokio::test]
async fn put_config_blank_key_keeps_existing() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let state = state_with_openai("sk-primary");
    let mut ui = state.ui_config.read().unwrap().clone();
    ui.llm.openai_api_key = String::new();
    for p in &mut ui.llm.providers {
        p.api_key = String::new();
    }

    let resp = put_config(
        State(state.clone()),
        Json(serde_json::to_value(&ui).expect("UiConfig must serialize")),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(stored_openai_key(&state), "sk-primary");
}

/// A real new key in a PUT replaces the stored one.
#[tokio::test]
async fn put_config_new_key_replaces_stored() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let state = state_with_openai("sk-old");
    let mut ui = state.ui_config.read().unwrap().clone();
    ui.llm.openai_api_key = "sk-new".to_string();
    ui.llm.providers[0].api_key = "sk-new".to_string();

    let resp = put_config(
        State(state.clone()),
        Json(serde_json::to_value(&ui).expect("UiConfig must serialize")),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(stored_openai_key(&state), "sk-new");
}

/// Partial-update regression: a sparse PUT (only `rules.minScore` present)
/// must update that field and keep every omitted field at its stored value —
/// not reset it to a serde default. The old `*ui = body` replaced the whole
/// config, so this request silently zeroed temperature / enableMetrics /
/// maxConcurrentReviews and dropped `llm.providers`.
#[tokio::test]
async fn put_config_sparse_patch_preserves_omitted_fields() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let state = state_with_openai("sk-primary");
    {
        let ui = state.ui_config.read().unwrap();
        assert_eq!(ui.rules.min_score, 75, "baseline min_score");
        assert_eq!(ui.llm.temperature, 0.7, "baseline temperature");
        assert!(ui.advanced.enable_metrics, "baseline enable_metrics");
        assert_eq!(ui.advanced.max_concurrent_reviews, 5, "baseline max concurrent");
        assert_eq!(ui.llm.providers.len(), 1, "baseline providers");
    }

    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({ "rules": { "minScore": 90 } })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    let ui = state.ui_config.read().unwrap();
    // Provided field updated...
    assert_eq!(ui.rules.min_score, 90);
    // ...every omitted field keeps its stored value.
    assert_eq!(ui.llm.temperature, 0.7, "omitted temperature must not reset");
    assert!(ui.advanced.enable_metrics, "omitted enableMetrics must not reset");
    assert_eq!(
        ui.advanced.max_concurrent_reviews, 5,
        "omitted maxConcurrentReviews must not reset"
    );
    assert_eq!(ui.llm.providers.len(), 1, "omitted providers must not be dropped");
    assert_eq!(ui.llm.openai_api_key, API_KEY_MASK, "stored key stays masked");
    assert_eq!(stored_openai_key(&state), "sk-primary", "live key preserved");
}

/// A sparse LLM patch must not clobber the stored key: with only
/// `llm.temperature` present, the merged config carries the masked key,
/// which resolves to the stored live key ("leave unchanged"), and the
/// provider list survives untouched.
#[tokio::test]
async fn put_config_sparse_llm_patch_keeps_key_and_providers() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let state = state_with_openai("sk-primary");
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({ "llm": { "temperature": 0.2 } })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    let ui = state.ui_config.read().unwrap();
    assert_eq!(ui.llm.temperature, 0.2);
    assert_eq!(ui.llm.openai_api_key, API_KEY_MASK);
    assert_eq!(ui.llm.providers.len(), 1);
    assert_eq!(stored_openai_key(&state), "sk-primary");
}

/// An empty object `{}` is the degenerate sparse case: a no-op save that
/// keeps every field, never a wipe.
#[tokio::test]
async fn put_config_empty_object_is_noop() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let state = state_with_openai("sk-primary");
    let before = state.ui_config.read().unwrap().clone();
    let resp = put_config(State(state.clone()), Json(serde_json::json!({})))
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let after = state.ui_config.read().unwrap().clone();
    assert_eq!(before.rules.min_score, after.rules.min_score);
    assert_eq!(before.llm.temperature, after.llm.temperature);
    assert_eq!(before.llm.providers.len(), after.llm.providers.len());
    assert_eq!(stored_openai_key(&state), "sk-primary");
}

/// A non-object or type-invalid update must be rejected with 422, not
/// silently accepted: `null`/`[]` are not a config patch, and a wrong-typed
/// field (e.g. `"minScore": "high"`) fails the merged deserialization.
#[tokio::test]
async fn put_config_malformed_update_rejected() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let state = state_with_openai("sk-primary");
    for payload in [
        serde_json::json!(null),
        serde_json::json!([]),
        serde_json::json!({ "rules": { "minScore": "high" } }),
    ] {
        let resp = put_config(State(state.clone()), Json(payload)).await.into_response();
        assert_eq!(
            resp.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "malformed patch must be rejected"
        );
    }
    // And nothing was persisted by any of the rejected patches.
    let ui = state.ui_config.read().unwrap();
    assert_eq!(ui.rules.min_score, 75);
    assert_eq!(ui.llm.temperature, 0.7);
    assert_eq!(stored_openai_key(&state), "sk-primary");
}

#[test]
fn is_blank_or_masked_treats_empty_and_mask_as_keep() {
    assert!(is_blank_or_masked(""));
    assert!(is_blank_or_masked(API_KEY_MASK));
    assert!(!is_blank_or_masked("sk-real"));
}

// ── POST /config/models probe key fallback ────────────────────────────

/// Build an `AppState` holding one LLM config with `api_base`/`api_key`,
/// mimicking an env-`LLM_CONFIG`-seeded server-side entry.
fn state_with_llm_entry(api_base: &str, api_key: &str) -> Arc<AppState> {
    Arc::new(AppState::new(vec![crate::models::LLMConfig {
        provider: "openai".to_string(),
        model: "gpt-4o".to_string(),
        api_key: api_key.to_string(),
        api_base: api_base.to_string(),
        max_tokens: 4096,
        temperature: 0.7,
        disable_thinking: None,
    }]))
}

async fn fetch_models_body(state: Arc<AppState>, api_base: &str, api_key: &str) -> serde_json::Value {
    let req: super::helpers::ModelsRequest =
        serde_json::from_value(serde_json::json!({ "api_base": api_base, "api_key": api_key }))
            .expect("ModelsRequest must deserialize");
    let resp = super::helpers::fetch_models(State(state), Json(req))
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Regression for the env-config 401: a masked (`***`) probe key must fall
/// back to the effective server-side key for the same api_base, so the
/// upstream provider authenticates instead of returning HTTP 401.
#[tokio::test]
async fn fetch_models_falls_back_to_server_key_when_masked() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("Authorization", "Bearer sk-real-server-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "mimo-2"}, {"id": "gpt-4o"}]
        })))
        .mount(&server)
        .await;

    let state = state_with_llm_entry(&server.uri(), "sk-real-server-key");
    let body = fetch_models_body(state, &server.uri(), API_KEY_MASK).await;
    assert_eq!(
        body["models"],
        serde_json::json!(["gpt-4o", "mimo-2"]),
        "the upstream request must carry the server-side key, got {body}"
    );
}

/// A blank probe key (frontend "leave blank") takes the same fallback.
#[tokio::test]
async fn fetch_models_falls_back_to_server_key_when_blank() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("Authorization", "Bearer sk-real-server-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "gpt-4o"}]
        })))
        .mount(&server)
        .await;

    let state = state_with_llm_entry(&server.uri(), "sk-real-server-key");
    let body = fetch_models_body(state, &server.uri(), "").await;
    assert_eq!(body["models"], serde_json::json!(["gpt-4o"]), "got {body}");
}

/// An explicit probe key is used as-is, even when the server holds a
/// different key for the same api_base (unchanged behavior).
#[tokio::test]
async fn fetch_models_uses_explicit_key_unchanged() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("Authorization", "Bearer sk-explicit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "custom-model"}]
        })))
        .mount(&server)
        .await;

    let state = state_with_llm_entry(&server.uri(), "sk-real-server-key");
    let body = fetch_models_body(state, &server.uri(), "sk-explicit").await;
    assert_eq!(body["models"], serde_json::json!(["custom-model"]), "got {body}");
}

/// A masked key with no matching server-side config keeps the old behavior:
/// the masked value is sent verbatim (and the provider's 401 surfaces as
/// the unchanged `{"models": [], "error": "HTTP 401 Unauthorized"}` shape).
#[tokio::test]
async fn fetch_models_without_matching_config_keeps_masked_key() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("Authorization", format!("Bearer {API_KEY_MASK}")))
        .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
        .mount(&server)
        .await;

    // The server-side entry points at a different api_base, so no fallback.
    let state = state_with_llm_entry("https://elsewhere.example/v1", "sk-real-server-key");
    let body = fetch_models_body(state, &server.uri(), API_KEY_MASK).await;
    assert_eq!(body["models"], serde_json::json!([]));
    assert_eq!(body["error"], "HTTP 401 Unauthorized");
}

// ── GitLab apiToken masking (contract-4) ──────────────────────────────

/// Snapshot/restore guard for the global GitLab runtime, so a round-trip
/// test can seed `gl_rt.token` without leaking state into the parallel
/// webhook-handler tests, which read the same global via
/// `effective_config`.
struct GitLabRuntimeGuard(crate::server::gitlab::GitLabRuntimeConfig);

impl GitLabRuntimeGuard {
    fn new() -> Self {
        Self(crate::server::gitlab::gitlab_runtime().read().unwrap().clone())
    }
}

impl Drop for GitLabRuntimeGuard {
    fn drop(&mut self) {
        let mut rt = crate::server::gitlab::gitlab_runtime().write().unwrap();
        *rt = self.0.clone();
    }
}

/// Every `put_config` call writes the global GitLab runtime (an empty
/// submitted `apiToken` clears it), so any test that drives `put_config`
/// races with the others on `gl_rt.token` — including `gitlab_api_token_mask_round_trip`,
/// whose keep/clear/replace assertions read that same global, and the
/// credential-resolution tests in `api::review::tests`.
///
/// Invariant: **every test that calls `put_config` or otherwise mutates the
/// runtime MUST take this lock** (async-aware, so it can be held across the
/// awaited handlers). The lock is shared crate-wide via
/// [`crate::server::gitlab::RUNTIME_TEST_LOCK`]. The `get_config`-only tests
/// never write the runtime and do not take it.
use crate::server::gitlab::RUNTIME_TEST_LOCK as GITLAB_RUNTIME_LOCK;

fn gitlab_runtime_token() -> String {
    crate::server::gitlab::gitlab_runtime().read().unwrap().token.clone()
}

async fn config_response_body(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Security regression: `GET /config` must never return the GitLab API
/// token in plaintext — a configured token comes back as the mask.
#[tokio::test]
async fn get_config_never_leaks_gitlab_api_token() {
    let state = state_with_openai("sk-super-secret");
    state.ui_config.write().unwrap().gitlab.api_token = "glpat-super-secret".to_string();

    let body = config_response_body(get_config(State(state)).await.into_response()).await;
    assert_eq!(body["gitlab"]["apiToken"], API_KEY_MASK);
    assert_ne!(body["gitlab"]["apiToken"], "glpat-super-secret");
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(
        !serialized.contains("glpat-super-secret"),
        "secret leaked in {serialized}"
    );
}

/// A token configured outside the UI (runtime only, e.g. CLI/env at
/// startup) is surfaced as the mask by GET, so the frontend never shows
/// "not set" for a configured token and an unrelated save cannot clear it.
#[tokio::test]
async fn get_config_surfaces_runtime_only_gitlab_token_as_mask() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    let state = state_with_openai("sk-primary");
    // ui_config carries no GitLab token; only the runtime does.
    crate::server::gitlab::gitlab_runtime().write().unwrap().token = "glpat-cli".to_string();

    let body = config_response_body(get_config(State(state)).await.into_response()).await;
    assert_eq!(body["gitlab"]["apiToken"], API_KEY_MASK);
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("glpat-cli"), "secret leaked in {serialized}");
}

/// Pure semantics: the mask sentinel `***` keeps the stored token.
#[test]
fn gitlab_runtime_token_resolution_keeps_on_mask() {
    let mut rt = crate::server::gitlab::GitLabRuntimeConfig {
        webhook_secret: String::new(),
        signing_secret: None,
        signing_key: None,
        token: "glpat-stored".to_string(),
    };
    let mut ui = UiGitLabConfig::default();
    ui.api_token = API_KEY_MASK.to_string();

    let resolved = apply_gitlab_runtime_config(&mut rt, &ui);
    assert_eq!(resolved, "glpat-stored");
    assert_eq!(rt.token, "glpat-stored");
}

/// Pure semantics: an empty string clears the stored token.
#[test]
fn gitlab_runtime_token_resolution_clears_on_empty() {
    let mut rt = crate::server::gitlab::GitLabRuntimeConfig {
        webhook_secret: String::new(),
        signing_secret: None,
        signing_key: None,
        token: "glpat-stored".to_string(),
    };
    let ui = UiGitLabConfig::default(); // api_token = ""

    let resolved = apply_gitlab_runtime_config(&mut rt, &ui);
    assert!(resolved.is_empty());
    assert!(rt.token.is_empty());
}

/// Pure semantics: a real value replaces the stored token.
#[test]
fn gitlab_runtime_token_resolution_replaces_on_real_value() {
    let mut rt = crate::server::gitlab::GitLabRuntimeConfig {
        webhook_secret: String::new(),
        signing_secret: None,
        signing_key: None,
        token: "glpat-old".to_string(),
    };
    let mut ui = UiGitLabConfig::default();
    ui.api_token = "glpat-new".to_string();

    let resolved = apply_gitlab_runtime_config(&mut rt, &ui);
    assert_eq!(resolved, "glpat-new");
    assert_eq!(rt.token, "glpat-new");
}

/// End-to-end round trip through `PUT /config` / `GET /config`: GET masks
/// the configured token, a masked (`***`) PUT keeps it, an empty PUT
/// clears it, a real PUT replaces it — and the plaintext never leaks.
#[tokio::test]
async fn gitlab_api_token_mask_round_trip() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    let state = state_with_openai("sk-primary");

    // Seed a configured GitLab token, as the runtime would hold it.
    crate::server::gitlab::gitlab_runtime().write().unwrap().token = "glpat-stored".to_string();
    state.ui_config.write().unwrap().gitlab.api_token = API_KEY_MASK.to_string();

    // 1. GET returns the mask, never the plaintext.
    let body = config_response_body(get_config(State(state.clone())).await.into_response()).await;
    assert_eq!(body["gitlab"]["apiToken"], API_KEY_MASK);
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("glpat-stored"), "secret leaked in {serialized}");

    // 2. PUT with `***` keeps the stored token.
    let mut ui = state.ui_config.read().unwrap().clone();
    ui.gitlab.api_token = API_KEY_MASK.to_string();
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::to_value(&ui).expect("UiConfig must serialize")),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(gitlab_runtime_token(), "glpat-stored");
    assert_eq!(state.ui_config.read().unwrap().gitlab.api_token, API_KEY_MASK);

    // 3. PUT with an empty string clears the token.
    let mut ui = state.ui_config.read().unwrap().clone();
    ui.gitlab.api_token = String::new();
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::to_value(&ui).expect("UiConfig must serialize")),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(gitlab_runtime_token().is_empty());
    assert!(state.ui_config.read().unwrap().gitlab.api_token.is_empty());

    // 4. PUT with a real token replaces it and is never echoed back.
    let mut ui = state.ui_config.read().unwrap().clone();
    ui.gitlab.api_token = "glpat-new".to_string();
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::to_value(&ui).expect("UiConfig must serialize")),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(gitlab_runtime_token(), "glpat-new");
    assert_eq!(state.ui_config.read().unwrap().gitlab.api_token, API_KEY_MASK);

    let body = config_response_body(get_config(State(state)).await.into_response()).await;
    assert_eq!(body["gitlab"]["apiToken"], API_KEY_MASK);
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("glpat-new"), "secret leaked in {serialized}");
}
