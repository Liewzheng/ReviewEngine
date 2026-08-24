//! REST API endpoints for the models.dev provider catalog.
//!
//! - `GET /api/v1/catalog/providers` — provider summaries (only providers
//!   carrying a usable HTTP `api` base), sorted by display name.
//! - `GET /api/v1/catalog/providers/{id}/models` — model summaries for one
//!   provider, sorted by name; 404 when the provider is unknown or SDK-only.
//!
//! Resolution order: fresh in-memory cache (24h TTL) → network fetch → stale
//! disk cache (`~/.config/review-engine/models-dev-cache.json`, overridable
//! via `REVIEW_MODELS_DEV_CACHE`) → stale in-memory cache → the builtin
//! static catalog ([`catalog::builtin_catalog`]). Successful fetches refresh
//! both caches. The endpoints never `502` on an upstream outage: the builtin
//! catalog exists precisely as the offline terminal fallback (e.g. Docker
//! deployments without egress to models.dev). Auth is applied by the parent
//! router like every other `/api/v1` endpoint.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use chrono::Utc;
use std::sync::Arc;
use std::time::Duration;

use crate::catalog::{self, Catalog, CatalogClient, CatalogModel, CatalogProvider, CatalogSource};
use crate::server::state::{CatalogCache, CatalogStore};
use crate::server::AppState;

/// In-memory TTL for the catalog — models.dev updates are not urgent, and
/// the document is fetched on every UI page load otherwise.
const CATALOG_CACHE_TTL: Duration = Duration::from_secs(24 * 3600);

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/providers", get(list_providers))
        .route("/providers/{id}/models", get(provider_models))
}

/// Return the in-memory catalog while it is still within the TTL. The read
/// guard is dropped before this function returns — it is never held across
/// an `.await`.
fn fresh_cached(store: &CatalogStore) -> Option<Arc<Catalog>> {
    let cache = store.cache.read().unwrap();
    cache.as_ref().and_then(|c| {
        if c.cached_at + CATALOG_CACHE_TTL > Utc::now() {
            Some(c.catalog.clone())
        } else {
            None
        }
    })
}

/// Resolve the catalog through the cache layers described in the module docs.
///
/// Infallible by design: these GET endpoints take no parameters, so there is
/// no malformed-request case to reject, and an unreachable upstream is a
/// deployment condition — not a client error. The terminal fallback is the
/// builtin static catalog, so the provider picker stays usable on air-gapped
/// or egress-restricted deployments instead of degrading to a 502.
async fn resolve_catalog(state: &AppState) -> Arc<Catalog> {
    // 1. Fresh in-memory cache.
    if let Some(catalog) = fresh_cached(&state.catalog) {
        return catalog;
    }

    // 2. Single-flight: exactly one request fetches from the network;
    //    concurrent requests on an expired cache queue on the fetch lock.
    let _fetch_guard = state.catalog.fetch_lock.lock().await;

    // 3. Double-check: a competitor may have refreshed while we queued.
    if let Some(catalog) = fresh_cached(&state.catalog) {
        return catalog;
    }

    // 4. Network fetch, with disk-cache fallback handled inside. The cache
    //    RwLock is not held across the network call — only the fetch lock.
    let result = match CatalogClient::from_env() {
        Ok(client) => {
            let cache_path = catalog::default_cache_path();
            catalog::fetch_or_disk_fallback(&client, cache_path.as_deref()).await
        }
        Err(e) => Err(e),
    };
    match result {
        Ok((catalog, source)) => {
            // A disk-fallback hit keeps its original fetch timestamp so the
            // TTL stays honest about the data's real age.
            let cached_at = match source {
                CatalogSource::Network => Utc::now(),
                CatalogSource::DiskCache(fetched_at) => fetched_at,
            };
            let catalog = Arc::new(catalog);
            *state.catalog.cache.write().unwrap() = Some(CatalogCache {
                catalog: catalog.clone(),
                cached_at,
            });
            catalog
        }
        Err(e) => {
            // 5. An expired in-memory entry still beats the static fallback.
            let stale = state.catalog.cache.read().unwrap().as_ref().map(|c| c.catalog.clone());
            if let Some(catalog) = stale {
                tracing::warn!("Catalog: fetch failed ({e}); serving stale in-memory cache");
                return catalog;
            }
            // 6. Terminal fallback: the builtin catalog. Deliberately NOT
            //    written into the caches, so the next request retries the
            //    network instead of pinning the static list behind the TTL.
            tracing::warn!("Catalog: fetch failed ({e}); serving builtin provider catalog");
            Arc::new(catalog::builtin_catalog())
        }
    }
}

/// The `GET /providers` summary shape. Only called for providers that carry
/// an `api` base (SDK-only entries are filtered out upstream).
fn provider_summary(p: &CatalogProvider) -> serde_json::Value {
    serde_json::json!({
        "id": p.id,
        "name": p.name,
        "api_base": p.api,
        "env": p.env,
        "doc": p.doc,
        "model_count": p.models.len(),
    })
}

/// The `GET /providers/{id}/models` summary shape.
fn model_summary(m: &CatalogModel) -> serde_json::Value {
    let (context_limit, output_limit) = match &m.limit {
        Some(limit) => (limit.context, limit.output),
        None => (None, None),
    };
    serde_json::json!({
        "id": m.id,
        "name": m.name,
        "context_limit": context_limit,
        "output_limit": output_limit,
        "reasoning": m.reasoning,
        "tool_call": m.tool_call,
    })
}

async fn list_providers(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let catalog = resolve_catalog(&state).await;
    let providers: Vec<serde_json::Value> = catalog::usable_providers(&catalog)
        .iter()
        .map(|p| provider_summary(p))
        .collect();
    Json(serde_json::json!({ "providers": providers }))
}

async fn provider_models(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> impl IntoResponse {
    let catalog = resolve_catalog(&state).await;
    match catalog.get(&id) {
        Some(provider) if provider.api.is_some() => {
            let models: Vec<serde_json::Value> = catalog::sorted_models(provider)
                .iter()
                .map(|m| model_summary(m))
                .collect();
            Json(serde_json::json!({ "models": models })).into_response()
        }
        _ => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("unknown provider: {id}") })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::state::CatalogCache;
    use axum::body::Body;
    use axum::http::Request;
    use chrono::Duration as ChronoDuration;
    use serde_json::json;
    use tower::ServiceExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_catalog_json() -> serde_json::Value {
        json!({
            "deepseek": {
                "id": "deepseek",
                "name": "DeepSeek",
                "npm": "@ai-sdk/openai-compatible",
                "env": ["DEEPSEEK_API_KEY"],
                "doc": "https://api-docs.deepseek.com/quick_start/pricing",
                "api": "https://api.deepseek.com",
                "models": {
                    "deepseek-chat": {
                        "id": "deepseek-chat",
                        "name": "DeepSeek Chat",
                        "reasoning": false,
                        "tool_call": true,
                        "limit": {"context": 64000, "output": 8192}
                    },
                    "deepseek-reasoner": {
                        "id": "deepseek-reasoner",
                        "name": "DeepSeek Reasoner",
                        "reasoning": true,
                        "limit": {"context": 128000}
                    }
                }
            },
            "zeta": {
                "id": "zeta",
                "name": "Zeta AI",
                "api": "https://api.zeta.example",
                "env": [],
                "models": {}
            },
            "sdk-only": {
                "id": "sdk-only",
                "name": "SDK Only",
                "npm": "@ai-sdk/amazon-bedrock",
                "models": {"m": {"id": "m", "name": "M"}}
            }
        })
    }

    fn test_catalog() -> Catalog {
        serde_json::from_value(test_catalog_json()).expect("test catalog parses")
    }

    /// State seeded with a fresh in-memory catalog cache, so handlers never
    /// touch the network.
    fn state_with_fresh_catalog() -> Arc<AppState> {
        let state = Arc::new(AppState::new(vec![]));
        *state.catalog.cache.write().unwrap() = Some(CatalogCache {
            catalog: Arc::new(test_catalog()),
            cached_at: Utc::now(),
        });
        state
    }

    async fn get_json(state: Arc<AppState>, uri: &str) -> (StatusCode, serde_json::Value) {
        let resp = routes()
            .with_state(state)
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    // ─── providers list ─────────────────────────────────────────

    #[tokio::test]
    async fn providers_list_matches_contract_shape() {
        let (status, body) = get_json(state_with_fresh_catalog(), "/providers").await;
        assert_eq!(status, StatusCode::OK);

        let providers = body["providers"].as_array().expect("providers array");
        // SDK-only provider excluded; remaining sorted by name.
        let ids: Vec<&str> = providers.iter().map(|p| p["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["deepseek", "zeta"]);

        let ds = &providers[0];
        assert_eq!(ds["name"], "DeepSeek");
        assert_eq!(ds["api_base"], "https://api.deepseek.com");
        assert_eq!(ds["env"], json!(["DEEPSEEK_API_KEY"]));
        assert_eq!(ds["doc"], "https://api-docs.deepseek.com/quick_start/pricing");
        assert_eq!(ds["model_count"], 2);

        // Optional fields serialize as null when absent.
        assert_eq!(providers[1]["doc"], serde_json::Value::Null);
        assert_eq!(providers[1]["env"], json!([]));
        assert_eq!(providers[1]["model_count"], 0);
    }

    // ─── provider models ────────────────────────────────────────

    #[tokio::test]
    async fn provider_models_matches_contract_shape() {
        let (status, body) = get_json(state_with_fresh_catalog(), "/providers/deepseek/models").await;
        assert_eq!(status, StatusCode::OK);

        let models = body["models"].as_array().expect("models array");
        let ids: Vec<&str> = models.iter().map(|m| m["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["deepseek-chat", "deepseek-reasoner"], "sorted by name");

        let chat = &models[0];
        assert_eq!(chat["name"], "DeepSeek Chat");
        assert_eq!(chat["context_limit"], 64000);
        assert_eq!(chat["output_limit"], 8192);
        assert_eq!(chat["reasoning"], false);
        assert_eq!(chat["tool_call"], true);

        // Missing optional fields serialize as null, not omitted.
        let reasoner = &models[1];
        assert_eq!(reasoner["context_limit"], 128000);
        assert_eq!(reasoner["output_limit"], serde_json::Value::Null);
        assert_eq!(reasoner["tool_call"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn provider_models_404_for_unknown_and_sdk_only() {
        let state = state_with_fresh_catalog();

        let (status, body) = get_json(state.clone(), "/providers/nope/models").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"].as_str().unwrap().contains("nope"));

        // Present in the catalog but carries no `api` base → same 404.
        let (status, _) = get_json(state, "/providers/sdk-only/models").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // ─── cache behavior ─────────────────────────────────────────

    /// Env guard: set both catalog env seams for a test, restoring afterwards.
    struct EnvGuard {
        base: Option<String>,
        cache: Option<String>,
    }

    impl EnvGuard {
        fn set(api_base: &str, cache_path: &std::path::Path) -> Self {
            let guard = Self {
                base: std::env::var(catalog::API_BASE_ENV).ok(),
                cache: std::env::var(catalog::CACHE_PATH_ENV).ok(),
            };
            std::env::set_var(catalog::API_BASE_ENV, api_base);
            std::env::set_var(catalog::CACHE_PATH_ENV, cache_path);
            guard
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.base {
                Some(v) => std::env::set_var(catalog::API_BASE_ENV, v),
                None => std::env::remove_var(catalog::API_BASE_ENV),
            }
            match &self.cache {
                Some(v) => std::env::set_var(catalog::CACHE_PATH_ENV, v),
                None => std::env::remove_var(catalog::CACHE_PATH_ENV),
            }
        }
    }

    fn state_with_expired_catalog() -> Arc<AppState> {
        let state = Arc::new(AppState::new(vec![]));
        *state.catalog.cache.write().unwrap() = Some(CatalogCache {
            catalog: Arc::new(Catalog::new()),
            cached_at: Utc::now() - ChronoDuration::hours(25),
        });
        state
    }

    #[tokio::test]
    async fn expired_memory_cache_refreshes_from_network_and_persists_disk() {
        // Mutates process env — must not interleave with other catalog tests.
        let _env_lock = crate::catalog::ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(test_catalog_json()))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let cache_path = dir.path().join("cache.json");
        let _env = EnvGuard::set(&server.uri(), &cache_path);

        let (status, body) = get_json(state_with_expired_catalog(), "/providers").await;
        assert_eq!(status, StatusCode::OK);
        let providers = body["providers"].as_array().expect("providers array");
        assert_eq!(providers.len(), 2);

        // Disk cache was written as a side effect.
        let disk = catalog::load_disk_cache(&cache_path).expect("disk cache written");
        assert!(disk.providers.contains_key("deepseek"));
    }

    #[tokio::test]
    async fn expired_memory_falls_back_to_stale_disk_on_fetch_failure() {
        // Mutates process env — must not interleave with other catalog tests.
        let _env_lock = crate::catalog::ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api.json"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let cache_path = dir.path().join("cache.json");
        catalog::write_disk_cache(&cache_path, &test_catalog()).expect("seed disk cache");
        let _env = EnvGuard::set(&server.uri(), &cache_path);

        // Note: an *expired but non-empty* in-memory cache would also serve;
        // here the in-memory entry is an empty catalog, and the disk cache is
        // what proves the fallback — the response must carry the disk data.
        let state = Arc::new(AppState::new(vec![]));
        let (status, body) = get_json(state, "/providers").await;
        assert_eq!(status, StatusCode::OK);
        let providers = body["providers"].as_array().expect("providers array");
        assert_eq!(providers.len(), 2, "stale disk cache must be served");
    }

    #[tokio::test]
    async fn fetch_failure_without_any_cache_serves_builtin_catalog() {
        // Mutates process env — must not interleave with other catalog tests.
        let _env_lock = crate::catalog::ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api.json"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let cache_path = dir.path().join("absent.json");
        let _env = EnvGuard::set(&server.uri(), &cache_path);

        // No fresh/expired memory entry, no disk cache, upstream down: the
        // endpoint must still answer 200 with the builtin catalog — an
        // unreachable upstream (e.g. Docker without egress) is not an error.
        let state = Arc::new(AppState::new(vec![]));
        let (status, body) = get_json(state.clone(), "/providers").await;
        assert_eq!(status, StatusCode::OK);
        let providers = body["providers"].as_array().expect("providers array");
        let ids: Vec<&str> = providers.iter().map(|p| p["id"].as_str().unwrap()).collect();
        assert!(
            ids.contains(&"openai") && ids.contains(&"deepseek"),
            "builtin catalog must be served: {ids:?}"
        );
        assert!(
            providers.iter().all(|p| p["api_base"].is_string()),
            "every builtin entry must carry an api base"
        );

        // The builtin fallback must not populate the cache: the next request
        // retries the network instead of pinning the static list for 24h.
        assert!(state.catalog.cache.read().unwrap().is_none());

        // A builtin provider resolves on the models endpoint with an empty
        // list — the UI then falls back to free-text model entry.
        let (status, body) = get_json(state.clone(), "/providers/deepseek/models").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["models"], json!([]));

        // An unknown id is still a genuine client error.
        let (status, _) = get_json(state, "/providers/nope/models").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn fresh_memory_cache_skips_network() {
        // No env override: if the handler touched the network it would hit
        // models.dev (or hang); a correct TTL short-circuit never does.
        let state = state_with_fresh_catalog();
        let (status, body) = get_json(state.clone(), "/providers").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["providers"].as_array().unwrap().len(), 2);

        // The seeded entry is still the one in the cache (unchanged timestamp
        // proves no refresh happened).
        let cache = state.catalog.cache.read().unwrap();
        let age = Utc::now() - cache.as_ref().unwrap().cached_at;
        assert!(age < ChronoDuration::minutes(1));
    }

    // ─── single-flight ──────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_expired_cache_fetches_upstream_exactly_once() {
        // Mutates process env — must not interleave with other catalog tests.
        let _env_lock = crate::catalog::ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        // A slow upstream widens the race window: without the fetch lock all
        // N requests would pile onto the mock instead of queueing behind one.
        Mock::given(method("GET"))
            .and(path("/api.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(test_catalog_json())
                    .set_delay(Duration::from_millis(250)),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let cache_path = dir.path().join("cache.json");
        let _env = EnvGuard::set(&server.uri(), &cache_path);

        // Empty in-memory cache: every request would independently fetch
        // without the single-flight gate.
        let state = Arc::new(AppState::new(vec![]));
        const N: usize = 8;
        let mut handles = Vec::with_capacity(N);
        for _ in 0..N {
            let state = state.clone();
            handles.push(tokio::spawn(async move { resolve_catalog(&state).await }));
        }
        for handle in handles {
            let catalog = handle.await.expect("task panicked");
            assert!(catalog.contains_key("deepseek"));
        }

        let received = server.received_requests().await.expect("request recording enabled");
        assert_eq!(
            received.len(),
            1,
            "single-flight must collapse {N} concurrent refreshes into one upstream request"
        );
    }

    #[tokio::test]
    async fn queued_request_serves_competitor_refresh_without_network() {
        // Mutates process env — must not interleave with other catalog tests.
        let _env_lock = crate::catalog::ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().expect("temp dir");
        let cache_path = dir.path().join("cache.json");
        let _env = EnvGuard::set(&server.uri(), &cache_path);

        let state = Arc::new(AppState::new(vec![]));

        // Hold the fetch lock so the resolve below queues behind it, then
        // simulate the competitor's refresh landing before the guard drops.
        let guard = state.catalog.fetch_lock.lock().await;
        let queued_state = state.clone();
        let queued = tokio::spawn(async move { resolve_catalog(&queued_state).await });

        tokio::task::yield_now().await;
        *state.catalog.cache.write().unwrap() = Some(CatalogCache {
            catalog: Arc::new(test_catalog()),
            cached_at: Utc::now(),
        });
        drop(guard);

        let catalog = queued.await.expect("task panicked");
        assert!(
            catalog.contains_key("deepseek"),
            "double-check must serve the fresh cache"
        );

        let received = server.received_requests().await.expect("request recording enabled");
        assert!(
            received.is_empty(),
            "a request that finds a fresh cache after the fetch lock must not hit the network"
        );
    }
}
