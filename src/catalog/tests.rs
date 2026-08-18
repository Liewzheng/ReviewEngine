use super::*;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A minimal but realistic catalog document covering the shapes the parser
/// must tolerate: a full provider, a provider with an SDK-only entry (no
/// `api`), sparse models, and unknown extra fields nested inside entries.
/// (Unknown *top-level* entries are a different case — see
/// `unknown_top_level_entries_parse_as_empty_providers`.)
fn catalog_json() -> serde_json::Value {
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
                    "attachment": false,
                    "reasoning": false,
                    "tool_call": true,
                    "release_date": "2024-05-01",
                    "cost": {"input": 0.27, "output": 1.1, "cache_read": 0.07},
                    "limit": {"context": 64000, "output": 8192},
                    "modalities": {"input": ["text"], "output": ["text"]},
                    "unknown_future_field": {"nested": true}
                },
                "deepseek-reasoner": {
                    "id": "deepseek-reasoner",
                    "name": "DeepSeek Reasoner",
                    "reasoning": true
                }
            },
            "extra_provider_field": 42
        },
        "sdk-only": {
            "id": "sdk-only",
            "name": "SDK Only",
            "npm": "@ai-sdk/amazon-bedrock",
            "env": ["AWS_ACCESS_KEY_ID"],
            "models": {}
        }
    })
}

// ─── parsing tolerance ──────────────────────────────────────────

#[test]
fn parses_realistic_catalog_tolerantly() {
    let catalog: Catalog = serde_json::from_value(catalog_json()).expect("catalog must parse");

    let ds = catalog.get("deepseek").expect("deepseek entry");
    assert_eq!(ds.id, "deepseek");
    assert_eq!(ds.name, "DeepSeek");
    assert_eq!(ds.api.as_deref(), Some("https://api.deepseek.com"));
    assert_eq!(ds.npm.as_deref(), Some("@ai-sdk/openai-compatible"));
    assert_eq!(ds.env, vec!["DEEPSEEK_API_KEY".to_string()]);
    assert_eq!(ds.models.len(), 2);

    let chat = ds.models.get("deepseek-chat").expect("chat model");
    assert!(chat.tool_call.unwrap());
    assert_eq!(chat.reasoning, Some(false));
    let limit = chat.limit.as_ref().expect("limit present");
    assert_eq!(limit.context, Some(64000));
    assert_eq!(limit.output, Some(8192));
    let cost = chat.cost.as_ref().expect("cost present");
    assert_eq!(cost.input, Some(0.27));
    assert_eq!(cost.output, Some(1.1));
    let modalities = chat.modalities.as_ref().expect("modalities present");
    assert_eq!(modalities.input, vec!["text".to_string()]);

    // Sparse model: everything optional must default.
    let reasoner = ds.models.get("deepseek-reasoner").expect("reasoner model");
    assert_eq!(reasoner.reasoning, Some(true));
    assert!(reasoner.limit.is_none());
    assert!(reasoner.cost.is_none());
    assert!(reasoner.tool_call.is_none());

    // SDK-only provider: no `api`.
    let sdk = catalog.get("sdk-only").expect("sdk-only entry");
    assert!(sdk.api.is_none());
}

#[test]
fn parses_provider_with_all_fields_missing() {
    let catalog: Catalog = serde_json::from_value(json!({"bare": {}})).expect("empty provider parses");
    let bare = catalog.get("bare").expect("bare entry");
    assert!(bare.id.is_empty());
    assert!(bare.api.is_none());
    assert!(bare.env.is_empty());
    assert!(bare.models.is_empty());
}

/// `Catalog` is a map of provider structs, so serde cannot "ignore" an
/// unknown top-level entry the way it ignores unknown struct fields: an
/// object value parses as an all-default provider. The graceful degradation
/// is that such entries carry no `api` and are filtered out of
/// [`usable_providers`].
#[test]
fn unknown_top_level_entries_parse_as_empty_providers_and_are_filtered() {
    let catalog: Catalog = serde_json::from_value(json!({
        "real": {"id": "real", "name": "Real", "api": "https://r.example", "models": {}},
        "future_entry": {"some_future_shape": true}
    }))
    .expect("catalog parses");
    assert_eq!(catalog.len(), 2, "unknown entry is kept as an empty provider");

    let future = catalog.get("future_entry").expect("future entry");
    assert!(future.api.is_none());

    let usable = usable_providers(&catalog);
    assert_eq!(usable.len(), 1);
    assert_eq!(usable[0].id, "real");
}

// ─── usable_providers / sorted_models ───────────────────────────

#[test]
fn usable_providers_skips_sdk_only_and_sorts_by_name() {
    let catalog: Catalog = serde_json::from_value(catalog_json()).expect("catalog parses");
    let providers = usable_providers(&catalog);
    assert_eq!(providers.len(), 1, "SDK-only provider must be excluded");
    assert_eq!(providers[0].id, "deepseek");

    let catalog: Catalog = serde_json::from_value(json!({
        "zeta": {"id": "zeta", "name": "Zeta", "api": "https://z.example", "models": {}},
        "Alpha": {"id": "Alpha", "name": "alpha", "api": "https://a.example", "models": {}},
        "mid": {"id": "mid", "name": "Mid", "models": {}}
    }))
    .expect("catalog parses");
    let names: Vec<&str> = usable_providers(&catalog).iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["alpha", "Zeta"],
        "case-insensitive name sort, no-api excluded"
    );
}

#[test]
fn sorted_models_orders_by_name() {
    let provider: CatalogProvider = serde_json::from_value(json!({
        "id": "p",
        "name": "P",
        "api": "https://p.example",
        "models": {
            "m-b": {"id": "m-b", "name": "Beta"},
            "m-a": {"id": "m-a", "name": "alpha"}
        }
    }))
    .expect("provider parses");
    let names: Vec<&str> = sorted_models(&provider).iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "Beta"]);
}

// ─── normalize_api_base ─────────────────────────────────────────

#[test]
fn normalize_appends_v1_for_openai_compatible_npm() {
    assert_eq!(
        normalize_api_base(Some("@ai-sdk/openai-compatible"), "https://api.deepseek.com"),
        "https://api.deepseek.com/v1"
    );
    assert_eq!(
        normalize_api_base(Some("@ai-sdk/openai"), "https://api.openai.com"),
        "https://api.openai.com/v1"
    );
}

#[test]
fn normalize_never_doubles_v1() {
    assert_eq!(
        normalize_api_base(Some("@ai-sdk/openai-compatible"), "https://api.deepseek.com/v1"),
        "https://api.deepseek.com/v1"
    );
    assert_eq!(
        normalize_api_base(Some("@ai-sdk/openai"), "https://api.openai.com/v1/"),
        "https://api.openai.com/v1"
    );
}

#[test]
fn normalize_trims_trailing_slash_before_suffix() {
    assert_eq!(
        normalize_api_base(Some("@ai-sdk/openai-compatible"), "https://api.groq.com/openai/"),
        "https://api.groq.com/openai/v1"
    );
}

#[test]
fn normalize_leaves_anthropic_and_unknown_npm_untouched() {
    // Anthropic is special-cased in ProviderRegistry; its native API takes no /v1.
    assert_eq!(
        normalize_api_base(Some("@ai-sdk/anthropic"), "https://api.anthropic.com"),
        "https://api.anthropic.com"
    );
    assert_eq!(
        normalize_api_base(Some("@ai-sdk/google"), "https://generativelanguage.googleapis.com"),
        "https://generativelanguage.googleapis.com"
    );
    assert_eq!(
        normalize_api_base(None, "https://llm.example.com/"),
        "https://llm.example.com"
    );
}

// ─── disk cache ─────────────────────────────────────────────────

#[test]
fn disk_cache_round_trips() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("nested").join("models-dev-cache.json");
    let catalog: Catalog = serde_json::from_value(catalog_json()).expect("catalog parses");

    write_disk_cache(&path, &catalog).expect("write cache");
    let loaded = load_disk_cache(&path).expect("cache loads");
    assert_eq!(loaded.providers.len(), catalog.len());
    assert_eq!(
        loaded.providers.get("deepseek").map(|p| p.name.as_str()),
        Some("DeepSeek")
    );
    // Parent directory was created on demand.
    assert!(path.exists());
    // No temp file left behind.
    assert!(!path.with_extension("tmp").exists());
}

#[test]
fn disk_cache_missing_or_corrupt_yields_none() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("nope.json");
    assert!(load_disk_cache(&missing).is_none());

    let corrupt = dir.path().join("corrupt.json");
    std::fs::write(&corrupt, "{not json").expect("write corrupt");
    assert!(load_disk_cache(&corrupt).is_none());
}

#[test]
fn cache_path_honors_env_override() {
    let _env_lock = ENV_LOCK.blocking_lock();
    let saved = std::env::var(CACHE_PATH_ENV).ok();

    std::env::set_var(CACHE_PATH_ENV, "/tmp/reng-catalog-test/cache.json");
    assert_eq!(
        default_cache_path(),
        Some(PathBuf::from("/tmp/reng-catalog-test/cache.json"))
    );

    std::env::remove_var(CACHE_PATH_ENV);
    let path = default_cache_path().expect("home-resolvable cache path");
    assert!(path.ends_with(".config/review-engine/models-dev-cache.json"));

    match saved {
        Some(v) => std::env::set_var(CACHE_PATH_ENV, v),
        None => {}
    }
}

// ─── client (wiremock) ──────────────────────────────────────────

#[tokio::test]
async fn client_fetches_catalog_with_versioned_user_agent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api.json"))
        .and(header(
            "User-Agent",
            format!("review-engine/{}", env!("CARGO_PKG_VERSION")),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(catalog_json()))
        .mount(&server)
        .await;

    let client = CatalogClient::with_base_url(&server.uri()).expect("client");
    let catalog = client.fetch_catalog().await.expect("fetch");
    assert_eq!(catalog.len(), 2);
    assert!(catalog.contains_key("deepseek"));
}

#[tokio::test]
async fn client_surfaces_http_error_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api.json"))
        .respond_with(ResponseTemplate::new(503).set_body_string("upstream down"))
        .mount(&server)
        .await;

    let client = CatalogClient::with_base_url(&server.uri()).expect("client");
    let err = client.fetch_catalog().await.expect_err("503 must error");
    let msg = err.to_string();
    assert!(msg.contains("503"), "error should mention status, got: {msg}");
    assert!(msg.contains("upstream down"), "error should carry body, got: {msg}");
}

#[tokio::test]
async fn client_rejects_malformed_json() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json at all"))
        .mount(&server)
        .await;

    let client = CatalogClient::with_base_url(&server.uri()).expect("client");
    assert!(client.fetch_catalog().await.is_err());
}

// ─── fetch_or_disk_fallback ─────────────────────────────────────

#[tokio::test]
async fn fallback_writes_disk_cache_on_network_success() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(catalog_json()))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("temp dir");
    let cache = dir.path().join("cache.json");
    let client = CatalogClient::with_base_url(&server.uri()).expect("client");

    let (catalog, source) = fetch_or_disk_fallback(&client, Some(&cache)).await.expect("fetch ok");
    assert_eq!(source, CatalogSource::Network);
    assert!(catalog.contains_key("deepseek"));

    // The disk cache was written as a side effect and round-trips.
    let disk = load_disk_cache(&cache).expect("disk cache written");
    assert!(disk.providers.contains_key("deepseek"));
}

#[tokio::test]
async fn fallback_serves_stale_disk_cache_on_fetch_failure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api.json"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    // Pre-seed a stale disk cache.
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = dir.path().join("cache.json");
    let seeded: Catalog = serde_json::from_value(catalog_json()).expect("catalog parses");
    write_disk_cache(&cache, &seeded).expect("seed cache");
    let seeded_at = load_disk_cache(&cache).expect("load seeded").fetched_at;

    let client = CatalogClient::with_base_url(&server.uri()).expect("client");
    let (catalog, source) = fetch_or_disk_fallback(&client, Some(&cache))
        .await
        .expect("stale disk cache must serve");
    assert!(catalog.contains_key("deepseek"));
    assert_eq!(source, CatalogSource::DiskCache(seeded_at));
}

#[tokio::test]
async fn fallback_errors_when_network_and_disk_both_fail() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api.json"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("temp dir");
    let cache = dir.path().join("absent.json");
    let client = CatalogClient::with_base_url(&server.uri()).expect("client");

    let err = fetch_or_disk_fallback(&client, Some(&cache))
        .await
        .expect_err("no disk cache must surface the fetch error");
    assert!(matches!(err, CatalogError::Api { status: 500, .. }));
}
