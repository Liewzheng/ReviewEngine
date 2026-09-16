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

/// Seed an `AppState` with the two-provider shape the RENG-72 regression was
/// measured on: two keyed providers, no `openai` among them — so the primary
/// is expressed by `ui.llm.primaryProvider` alone, never by the legacy
/// scalar path. (RENG-75 identity batch: the stored model matches the card
/// payload below, because a masked key now follows the
/// `(provider, api_base, model)` triple — a model edit with a masked key
/// clears the key by design, see `put_config_model_edit_with_masked_key_clears_the_key`.)
fn state_with_two_providers() -> Arc<AppState> {
    let app: crate::models::AppConfig = serde_json::from_value(serde_json::json!({
        "llm": [
            {
                "provider": "xiaomi-token-plan-cn",
                "model": "mimo",
                "api_key": "sk-xiaomi",
                "api_base": "http://xiaomi.invalid/v1",
                "max_tokens": 4096,
                "temperature": 0.7
            },
            {
                "provider": "deepseek",
                "model": "deepseek-v4-flash",
                "api_key": "sk-deepseek",
                "api_base": "https://api.deepseek.com/v1",
                "max_tokens": 4096,
                "temperature": 0.7
            }
        ]
    }))
    .expect("two-provider AppConfig must deserialize");
    let state = Arc::new(AppState::new(app.llm.clone()));
    *state.app_config.write().unwrap() = Some(Arc::new(app.clone()));
    *state.ui_config.write().unwrap() = UiConfig::from_app_config(&app);
    state
}

/// The payload the provider-card UI sends for an ordinary add/edit: every
/// card (masked keys) and NO `primaryProvider` — an edit does not speak for
/// the primary (RENG-72). `deepseek` is the card being edited here.
fn card_edit_payload(max_tokens: u32) -> serde_json::Value {
    serde_json::json!({
        "llm": {
            "providers": [
                {
                    "provider": "xiaomi-token-plan-cn",
                    "apiKey": API_KEY_MASK,
                    "apiBaseUrl": "http://xiaomi.invalid/v1",
                    "defaultModel": "mimo",
                    "maxTokens": 4096,
                    "temperature": 0.7,
                    "timeoutSeconds": 60,
                    "retryAttempts": 3
                },
                {
                    "provider": "deepseek",
                    "apiKey": API_KEY_MASK,
                    "apiBaseUrl": "https://api.deepseek.com/v1",
                    "defaultModel": "deepseek-v4-flash",
                    "maxTokens": max_tokens,
                    "temperature": 0.7,
                    "timeoutSeconds": 60,
                    "retryAttempts": 3
                }
            ]
        }
    })
}

/// RENG-72 regression: an ordinary card edit must not move the primary. The
/// measured failure was a card save re-asserting the primary its (older) view
/// held, dropping the user's choice back to the array head — the UI now omits
/// `primaryProvider` from such a save, and an omitted key keeps the stored
/// value. The edited card itself still applies, live key included.
#[tokio::test]
async fn put_config_card_edit_without_primary_keeps_the_stored_primary() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let state = state_with_two_providers();
    // The user's explicit choice: deepseek becomes primary.
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({ "llm": { "primaryProvider": "deepseek" } })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(state.ui_config.read().unwrap().llm.primary_provider, "deepseek");

    let resp = put_config(State(state.clone()), Json(card_edit_payload(2048)))
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    let ui = state.ui_config.read().unwrap();
    assert_eq!(
        ui.llm.primary_provider, "deepseek",
        "a card edit must not drag the primary back to the array head"
    );
    let deepseek = ui
        .llm
        .providers
        .iter()
        .find(|p| p.provider == "deepseek")
        .expect("deepseek card survives the save");
    assert_eq!(deepseek.max_tokens, 2048, "the edit is applied");
    assert_eq!(deepseek.default_model, "deepseek-v4-flash");
    assert_eq!(deepseek.api_key, API_KEY_MASK, "masked-keep, never a live key");
    drop(ui);

    let live = state.llm_configs.read().unwrap();
    let entry = live
        .iter()
        .find(|c| c.provider == "deepseek")
        .expect("deepseek stays configured");
    assert_eq!(entry.max_tokens, 2048);
    assert_eq!(entry.model, "deepseek-v4-flash");
    assert_eq!(entry.api_key, "sk-deepseek", "stored key kept across the save");
}

/// The other half of the contract: a save that DOES carry the primary choice
/// still moves it — omitting the primary on ordinary saves must not turn
/// "set as primary" into a silent no-op, and the persisted primary is what
/// leads the runtime chain (RENG-55).
#[tokio::test]
async fn put_config_explicit_primary_choice_still_wins() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let state = state_with_two_providers();
    assert_eq!(
        state.ui_config.read().unwrap().llm.primary_provider,
        "xiaomi-token-plan-cn"
    );

    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({ "llm": { "primaryProvider": "deepseek" } })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(state.ui_config.read().unwrap().llm.primary_provider, "deepseek");
    assert_eq!(
        state.ordered_llm_configs()[0].provider,
        "deepseek",
        "the persisted primary leads the runtime chain"
    );
}

/// RENG-75: `disabled` round-trips through `PUT /config`: setting it persists
/// (runtime set + masked UI echo), an unrelated save that never mentions the
/// field keeps it, a providers[] save that OMITS the key keeps it too (the
/// masked-keep equivalent — `None` means "not spoken for"), and an explicit
/// `false` re-enables with the configuration intact.
#[tokio::test]
async fn put_config_disabled_round_trips_and_unspoken_saves_keep_it() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let state = state_with_two_providers();

    // 1) Disable deepseek explicitly (the payload the LLM page sends).
    let mut payload = card_edit_payload(4096);
    payload["llm"]["providers"][1]["disabled"] = serde_json::json!(true);
    let resp = put_config(State(state.clone()), Json(payload)).await.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    {
        let live = state.llm_configs.read().unwrap();
        let deepseek = live.iter().find(|c| c.provider == "deepseek").unwrap();
        assert!(deepseek.disabled, "the flag must reach the runtime set");
        let xiaomi = live.iter().find(|c| c.provider == "xiaomi-token-plan-cn").unwrap();
        assert!(!xiaomi.disabled);
    }
    {
        let ui = state.ui_config.read().unwrap();
        let deepseek = ui.llm.providers.iter().find(|p| p.provider == "deepseek").unwrap();
        assert_eq!(deepseek.disabled, Some(true), "GET /config echoes a concrete bool");
        assert_eq!(deepseek.api_key, API_KEY_MASK, "masked-keep still applies");
    }
    // The chain skips the disabled entry: xiaomi alone remains.
    assert_eq!(
        state
            .ordered_llm_configs()
            .iter()
            .map(|c| c.provider.as_str())
            .collect::<Vec<_>>(),
        vec!["xiaomi-token-plan-cn"]
    );

    // 2) An unrelated save (no `llm` key at all) keeps the flag.
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({ "rules": { "minScore": 90 } })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        state
            .llm_configs
            .read()
            .unwrap()
            .iter()
            .find(|c| c.provider == "deepseek")
            .unwrap()
            .disabled,
        "an unrelated save must not re-enable the provider"
    );

    // 3) A card edit whose entries OMIT `disabled` (an older client's shape)
    //    keeps it too — the same keep semantics as the masked API key.
    let resp = put_config(State(state.clone()), Json(card_edit_payload(2048)))
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    {
        let live = state.llm_configs.read().unwrap();
        let deepseek = live.iter().find(|c| c.provider == "deepseek").unwrap();
        assert!(deepseek.disabled, "an omitted `disabled` key keeps the stored flag");
        assert_eq!(deepseek.max_tokens, 2048, "the edit itself applies");
    }

    // 4) An explicit `false` re-enables — the configuration was fully kept.
    let mut payload = card_edit_payload(2048);
    payload["llm"]["providers"][1]["disabled"] = serde_json::json!(false);
    let resp = put_config(State(state.clone()), Json(payload)).await.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    {
        let live = state.llm_configs.read().unwrap();
        let deepseek = live.iter().find(|c| c.provider == "deepseek").unwrap();
        assert!(!deepseek.disabled, "explicit false re-enables");
        assert_eq!(
            deepseek.api_key, "sk-deepseek",
            "the stored key survived the disable cycle"
        );
        assert_eq!(deepseek.api_base, "https://api.deepseek.com/v1");
    }
    assert_eq!(state.ordered_llm_configs().len(), 2, "back in the chain");
}

/// RENG-75: disabling the recorded primary normalises the echo to the first
/// ENABLED provider — the effective head is never a disabled entry. (RENG-72
/// is the other half: a primary that still names an enabled entry is never
/// rewritten by a save that does not speak for it.)
#[tokio::test]
async fn put_config_disabling_the_primary_moves_the_recorded_primary_to_the_first_enabled() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let state = state_with_two_providers();
    // The stored primary is the array head (from_app_config's default).
    assert_eq!(
        state.ui_config.read().unwrap().llm.primary_provider,
        "xiaomi-token-plan-cn"
    );

    let mut payload = card_edit_payload(4096);
    payload["llm"]["providers"][0]["disabled"] = serde_json::json!(true);
    let resp = put_config(State(state.clone()), Json(payload)).await.into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    assert_eq!(
        state.ui_config.read().unwrap().llm.primary_provider,
        "deepseek",
        "the recorded primary follows to the first enabled provider"
    );
    assert_eq!(
        state.ordered_llm_configs()[0].provider,
        "deepseek",
        "and the chain head agrees"
    );

    // Disabling EVERY provider empties the echo (order stays authoritative)
    // and the chain.
    let mut payload = card_edit_payload(4096);
    payload["llm"]["providers"][0]["disabled"] = serde_json::json!(true);
    payload["llm"]["providers"][1]["disabled"] = serde_json::json!(true);
    let resp = put_config(State(state.clone()), Json(payload)).await.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(state.ui_config.read().unwrap().llm.primary_provider, "");
    assert!(state.ordered_llm_configs().is_empty(), "no enabled provider, no chain");
}

/// RENG-77 §1: `disable_thinking` is unreachable from the Web UI — the
/// UI's typed payload never carried the field, and the PUT pipeline
/// hardcoded `disable_thinking: None` when rebuilding the
/// `LLMConfig`. Two UAT deepseek reviews hit exactly this: 11 experts
/// with empty findings, perfect 100, every `max_tokens` spent on
/// `reasoning_tokens`. This test pins the fix: the field round-trips
/// through `PUT /config` and is echoed back on `GET /config`, an
/// unrelated save keeps it, a save that omits it keeps the stored
/// value, and the request-time config the LLMClient sees carries the
/// flag (so the request body inlines `"thinking": {"type": "disabled"}`).
#[tokio::test]
async fn put_config_disable_thinking_round_trips_and_unspoken_saves_keep_it() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let state = state_with_two_providers();

    // 1) An explicit `true` on the deepseek card reaches the runtime set
    //    AND is echoed back through `UiConfig::from_app_config`.
    let mut payload = card_edit_payload(4096);
    payload["llm"]["providers"][1]["disableThinking"] = serde_json::json!(true);
    let resp = put_config(State(state.clone()), Json(payload)).await.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    {
        let live = state.llm_configs.read().unwrap();
        let deepseek = live.iter().find(|c| c.provider == "deepseek").unwrap();
        assert_eq!(
            deepseek.disable_thinking,
            Some(true),
            "the flag must reach the runtime set (RENG-77 §1)"
        );
        let xiaomi = live.iter().find(|c| c.provider == "xiaomi-token-plan-cn").unwrap();
        assert_eq!(
            xiaomi.disable_thinking, None,
            "the other card is unchanged: tri-state survives a single-card edit"
        );
    }
    {
        let ui = state.ui_config.read().unwrap();
        let deepseek = ui.llm.providers.iter().find(|p| p.provider == "deepseek").unwrap();
        assert_eq!(
            deepseek.disable_thinking,
            Some(true),
            "GET /config echoes the stored value as a concrete Option<bool>"
        );
    }

    // 2) An unrelated save (no `llm` key at all) keeps the flag — same rule
    //    as the masked API key and the disabled flag: a partial save that
    //    does not speak for the field cannot silently revert it.
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({ "rules": { "minScore": 90 } })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    {
        let live = state.llm_configs.read().unwrap();
        let deepseek = live.iter().find(|c| c.provider == "deepseek").unwrap();
        assert_eq!(
            deepseek.disable_thinking,
            Some(true),
            "an unrelated save must not turn the flag back off"
        );
    }

    // 3) A card edit whose entries OMIT `disableThinking` keeps it too —
    //    the masked-keep equivalent, mirroring the `disabled` field's test.
    let resp = put_config(State(state.clone()), Json(card_edit_payload(2048)))
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    {
        let live = state.llm_configs.read().unwrap();
        let deepseek = live.iter().find(|c| c.provider == "deepseek").unwrap();
        assert_eq!(
            deepseek.disable_thinking,
            Some(true),
            "an omitted `disableThinking` key keeps the stored tri-state"
        );
        assert_eq!(deepseek.max_tokens, 2048, "the unrelated edit itself still applies");
    }

    // 4) An explicit `false` re-enables thinking — the configuration was
    //    fully kept.
    let mut payload = card_edit_payload(2048);
    payload["llm"]["providers"][1]["disableThinking"] = serde_json::json!(false);
    let resp = put_config(State(state.clone()), Json(payload)).await.into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    {
        let live = state.llm_configs.read().unwrap();
        let deepseek = live.iter().find(|c| c.provider == "deepseek").unwrap();
        assert_eq!(
            deepseek.disable_thinking,
            Some(false),
            "explicit false sets the tri-state to false (not None — they are distinct)"
        );
    }
    {
        let ui = state.ui_config.read().unwrap();
        let deepseek = ui.llm.providers.iter().find(|p| p.provider == "deepseek").unwrap();
        assert_eq!(deepseek.disable_thinking, Some(false), "GET /config echoes Some(false)");
    }

    // 5) A subsequent save that OMITS the field again keeps `Some(false)` —
    //    the tri-state is preserved across saves, not collapsed to None.
    let resp = put_config(State(state.clone()), Json(card_edit_payload(8192)))
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        state
            .llm_configs
            .read()
            .unwrap()
            .iter()
            .find(|c| c.provider == "deepseek")
            .unwrap()
            .disable_thinking,
        Some(false),
        "tri-state survives another omitted-key save"
    );
}

// ─── RENG-75 (identity batch): same-named cards & entry-following keep ───

/// Seed an `AppState` with the DANGEROUS shape: two cards sharing the
/// `(provider, api_base, model)` triple — two accounts of one service —
/// differing in `api_key`, plus one non-identity field (`max_tokens`) so the
/// two cards are distinguishable in a masked payload. A name- (or even
/// triple-)based keep cannot tell them apart; only the index rule can, and
/// only a payload that differs per card can prove the order survived.
fn state_with_two_accounts() -> Arc<AppState> {
    let account = |key: &str, max_tokens: u32| {
        serde_json::json!({
            "provider": "acme-pay",
            "model": "m1",
            "api_key": key,
            "api_base": "https://api.acme.example/v1",
            "max_tokens": max_tokens,
            "temperature": 0.7
        })
    };
    let app: crate::models::AppConfig = serde_json::from_value(serde_json::json!({
        "llm": [account("sk-acct-a", 4096), account("sk-acct-b", 2048)]
    }))
    .expect("two-account AppConfig must deserialize");
    let state = Arc::new(AppState::new(app.llm.clone()));
    *state.app_config.write().unwrap() = Some(Arc::new(app.clone()));
    *state.ui_config.write().unwrap() = UiConfig::from_app_config(&app);
    state
}

/// A masked providers[] entry for one acme account (what the UI submits when
/// the user edits a card without touching the key field).
fn account_card(max_tokens: u32) -> serde_json::Value {
    serde_json::json!({
        "provider": "acme-pay",
        "apiKey": API_KEY_MASK,
        "apiBaseUrl": "https://api.acme.example/v1",
        "defaultModel": "m1",
        "maxTokens": max_tokens,
        "temperature": 0.7,
        "timeoutSeconds": 60,
        "retryAttempts": 3
    })
}

fn live_keys(state: &Arc<AppState>) -> Vec<String> {
    state
        .llm_configs
        .read()
        .unwrap()
        .iter()
        .map(|c| c.api_key.clone())
        .collect()
}

/// THE dangerous case: editing the SECOND of two same-triple accounts with a
/// masked key must keep the second account's OWN key — the pre-fix,
/// name-based keep would have re-stamped it with the FIRST account's key.
#[tokio::test]
async fn put_config_masked_edit_keeps_each_accounts_own_key() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let state = state_with_two_accounts();
    assert_eq!(live_keys(&state), vec!["sk-acct-a", "sk-acct-b"]);

    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({ "llm": { "providers": [account_card(4096), account_card(1024)] } })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    assert_eq!(
        live_keys(&state),
        vec!["sk-acct-a", "sk-acct-b"],
        "each account keeps its own key — never the sibling's"
    );
    assert_eq!(
        state.llm_configs.read().unwrap()[1].max_tokens,
        1024,
        "the edit applies"
    );
    // The masked echo is per-entry too: both cards show configured.
    let ui = state.ui_config.read().unwrap();
    assert!(ui.llm.providers.iter().all(|p| p.api_key == API_KEY_MASK));
}

/// The same two accounts reordered and saved with masked keys: both keys
/// survive, exactly once each. Identical-triple cards are byte-identical in a
/// masked payload, so identity follows POSITION (RENG-75 追加确认 3: card
/// identity = storage index) — the one outcome that can never duplicate or
/// drop an account's secret.
#[tokio::test]
async fn put_config_swapping_two_accounts_preserves_both_keys() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let state = state_with_two_accounts();

    let mut swapped = account_card(2048);
    swapped["disabled"] = serde_json::json!(true); // make the swap visible
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({ "llm": { "providers": [swapped, account_card(4096)] } })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    let mut keys = live_keys(&state);
    keys.sort();
    assert_eq!(
        keys,
        vec!["sk-acct-a", "sk-acct-b"],
        "both accounts survive — no key duplicated, none dropped"
    );
}

/// A reorder of cards with DIFFERENT triples moves each key with its card
/// (the unique-triple fallback of the keep rule): after swapping xiaomi and
/// deepseek, xiaomi's key sits with xiaomi's card and deepseek's with
/// deepseek's.
#[tokio::test]
async fn put_config_reorder_different_triples_moves_keys_with_their_cards() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let state = state_with_two_providers();

    let mut payload = card_edit_payload(4096);
    let providers = payload["llm"]["providers"].as_array_mut().unwrap();
    providers.swap(0, 1);
    let resp = put_config(State(state.clone()), Json(payload)).await.into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    let live = state.llm_configs.read().unwrap();
    assert_eq!(live[0].provider, "deepseek", "the new order is stored");
    assert_eq!(live[0].api_key, "sk-deepseek", "deepseek's key followed its card");
    assert_eq!(live[1].provider, "xiaomi-token-plan-cn");
    assert_eq!(live[1].api_key, "sk-xiaomi", "xiaomi's key followed its card");
}

/// The full same-name round trip through the real endpoints: `GET /config`
/// echoes both same-named cards, and re-submitting that exact array through
/// `PUT /config` (twice) keeps the order and each entry's own key — no
/// name-based merge collapses or reorders the cards.
#[tokio::test]
async fn put_config_same_name_cards_round_trip_without_name_merging() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let state = state_with_two_accounts();

    // `maxTokens` is the only field the masked payload shows differently per
    // card, so it is what proves the ORDER survived (the keys are masked).
    let order = |providers: &[serde_json::Value]| -> Vec<Option<u64>> {
        providers.iter().map(|p| p["maxTokens"].as_u64()).collect()
    };
    let providers = config_response_body(get_config(State(state.clone())).await.into_response()).await["llm"]
        ["providers"]
        .as_array()
        .expect("llm.providers array")
        .clone();
    assert_eq!(providers.len(), 2, "GET /config echoes both same-named cards");
    assert_eq!(
        order(&providers),
        vec![Some(4096), Some(2048)],
        "stored order, no reordering"
    );
    assert!(
        providers.iter().all(|p| p["apiKey"] == API_KEY_MASK),
        "both cards show the mask, never a live key: {providers:?}"
    );

    for round in 1..=2 {
        let resp = put_config(
            State(state.clone()),
            Json(serde_json::json!({ "llm": { "providers": providers.clone() } })),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK, "PUT /config round {round}");

        let after = config_response_body(get_config(State(state.clone())).await.into_response()).await["llm"]
            ["providers"]
            .as_array()
            .expect("llm.providers array")
            .clone();
        assert_eq!(after.len(), 2, "round {round}: no name-based merge collapsed the cards");
        assert_eq!(
            order(&after),
            vec![Some(4096), Some(2048)],
            "round {round}: order is untouched"
        );
        assert!(
            after.iter().all(|p| p["apiKey"] == API_KEY_MASK),
            "round {round}: both cards stay configured"
        );
        assert_eq!(
            live_keys(&state),
            vec!["sk-acct-a", "sk-acct-b"],
            "round {round}: each key stays with its own card"
        );
        let names: Vec<String> = state
            .llm_configs
            .read()
            .unwrap()
            .iter()
            .map(|c| c.provider.clone())
            .collect();
        assert_eq!(
            names,
            vec!["acme-pay", "acme-pay"],
            "round {round}: the shared name stays"
        );
    }
}

/// The strict edge of the identity rule (RENG-75): the key is part of the
/// card's identity, so editing a card's MODEL (or URL) with a masked key
/// keeps nothing — the entry resolves to empty and needs a re-entered key,
/// exactly like a git platform whose baseUrl changed. The sibling card is
/// untouched.
#[tokio::test]
async fn put_config_model_edit_with_masked_key_clears_the_key() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let state = state_with_two_accounts();

    let mut edited = account_card(2048);
    edited["defaultModel"] = serde_json::json!("m2"); // model change, key untouched
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({ "llm": { "providers": [account_card(4096), edited] } })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    {
        let live = state.llm_configs.read().unwrap();
        assert_eq!(live.len(), 1, "the keyless edited card cannot stay in the live set");
        assert_eq!(live[0].api_key, "sk-acct-a", "the untouched card keeps its key");
    }

    // Re-entering the key on the edited card restores it as its own card.
    let mut rekeyed = account_card(2048);
    rekeyed["defaultModel"] = serde_json::json!("m2");
    rekeyed["apiKey"] = serde_json::json!("sk-acct-b2");
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({ "llm": { "providers": [account_card(4096), rekeyed] } })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let live = state.llm_configs.read().unwrap();
    assert_eq!(live.len(), 2);
    assert_eq!(live[1].model, "m2");
    assert_eq!(live[1].api_key, "sk-acct-b2", "the new key is stored, not the old one");
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
        disabled: false,
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

// ── gitPlatforms (multi-instance) ─────────────────────────────────

fn testbed_platform_json() -> serde_json::Value {
    serde_json::json!({
        "name": "testbed",
        "type": "gitlab",
        "baseUrl": "http://gitlab.internal:8929",
        "token": "glpat-platform",
        "webhookSecret": "wh-platform"
    })
}

fn stored_platform(state: &Arc<AppState>, name: &str) -> Option<crate::models::GitPlatformConfig> {
    state
        .git_platforms
        .read()
        .unwrap()
        .iter()
        .find(|p| p.name == name)
        .cloned()
}

/// PUT/GET round trip: the entry lands in the live store with real secrets,
/// and GET returns the masked projection — never a live secret.
#[tokio::test]
async fn put_get_git_platforms_round_trip() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    let state = state_with_openai("sk-primary");

    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({ "gitPlatforms": [testbed_platform_json()] })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    let stored = stored_platform(&state, "testbed").expect("platform must be stored");
    assert_eq!(stored.platform_type, "gitlab");
    assert_eq!(stored.base_url, "http://gitlab.internal:8929");
    assert_eq!(stored.token, "glpat-platform");
    assert_eq!(stored.webhook_secret, "wh-platform");

    let body = config_response_body(get_config(State(state.clone())).await.into_response()).await;
    let entry = &body["gitPlatforms"][0];
    assert_eq!(entry["name"], "testbed");
    assert_eq!(entry["type"], "gitlab");
    assert_eq!(entry["baseUrl"], "http://gitlab.internal:8929");
    assert_eq!(entry["token"], API_KEY_MASK);
    assert_eq!(entry["webhookSecret"], API_KEY_MASK);
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("glpat-platform"), "secret leaked in {serialized}");
    assert!(!serialized.contains("wh-platform"), "secret leaked in {serialized}");
}

/// `allowedProjects` (camelCase) is carried through PUT → stored model and
/// back to GET, sanitized: entries are trimmed, blank/whitespace-only items
/// dropped, duplicates removed (first occurrence wins, order preserved). No
/// format validation — any non-empty string is kept.
#[tokio::test]
async fn put_git_platforms_allowed_projects_sanitized_and_round_trips() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    let state = state_with_openai("sk-primary");

    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({
            "gitPlatforms": [{
                "name": "testbed",
                "type": "gitlab",
                "baseUrl": "http://gitlab.internal:8929",
                "token": "glpat-platform",
                "webhookSecret": "wh-platform",
                "allowedProjects": [" group/a ", "   ", "", "group/b", "group/a", "group/b", "group/c "]
            }]
        })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    let stored = stored_platform(&state, "testbed").expect("platform must be stored");
    assert_eq!(
        stored.allowed_projects,
        vec!["group/a", "group/b", "group/c"],
        "trimmed, de-duplicated, blank entries dropped"
    );

    // GET projects the allowlist back in camelCase (omitted field default: []).
    let body = config_response_body(get_config(State(state.clone())).await.into_response()).await;
    let entry = &body["gitPlatforms"][0];
    assert_eq!(
        entry["allowedProjects"],
        serde_json::json!(["group/a", "group/b", "group/c"])
    );
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(
        serialized.contains("allowedProjects"),
        "camelCase field missing: {serialized}"
    );
}

/// Masked (`***`) or blank secrets on an entry with the SAME (name, baseUrl)
/// keep the stored secret; a real value replaces it.
#[tokio::test]
async fn put_git_platforms_masked_secret_keeps_stored() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    let state = state_with_openai("sk-primary");

    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({ "gitPlatforms": [testbed_platform_json()] })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    // Same name + same baseUrl: a masked token + blank webhookSecret keep
    // both stored secrets.
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({
            "gitPlatforms": [{
                "name": "testbed",
                "type": "gitlab",
                "baseUrl": "http://gitlab.internal:8929",
                "token": API_KEY_MASK,
                "webhookSecret": ""
            }]
        })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let stored = stored_platform(&state, "testbed").unwrap();
    assert_eq!(stored.token, "glpat-platform", "masked token keeps stored secret");
    assert_eq!(
        stored.webhook_secret, "wh-platform",
        "blank webhookSecret keeps stored secret"
    );
    assert_eq!(stored.base_url, "http://gitlab.internal:8929");

    // A real value replaces the stored secret.
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({
            "gitPlatforms": [{
                "name": "testbed",
                "type": "gitlab",
                "baseUrl": "http://gitlab.internal:8929",
                "token": "glpat-new"
            }]
        })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(stored_platform(&state, "testbed").unwrap().token, "glpat-new");
}

/// Secret-keep matches on the (name, baseUrl) PAIR: re-pointing the
/// same-named entry at a DIFFERENT instance must NOT inherit the old
/// instance's credentials (cross-instance secret leakage). The entry is
/// saved with empty secrets; the user re-enters them explicitly.
#[tokio::test]
async fn put_git_platforms_base_url_change_does_not_inherit_secrets() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    let state = state_with_openai("sk-primary");

    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({ "gitPlatforms": [testbed_platform_json()] })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    // Same name, different baseUrl, masked secrets → secrets are NOT carried
    // over from the other instance.
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({
            "gitPlatforms": [{
                "name": "testbed",
                "type": "gitlab",
                "baseUrl": "http://gitlab.internal:9000",
                "token": API_KEY_MASK,
                "webhookSecret": API_KEY_MASK
            }]
        })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let stored = stored_platform(&state, "testbed").unwrap();
    assert_eq!(stored.base_url, "http://gitlab.internal:9000");
    assert!(
        stored.token.is_empty(),
        "changed baseUrl must not inherit the old instance's token, got {:?}",
        stored.token
    );
    assert!(
        stored.webhook_secret.is_empty(),
        "changed baseUrl must not inherit the old instance's webhook secret"
    );

    // The GET projection shows empty (unconfigured) secrets — not a `***`
    // mask that would imply a stored secret exists.
    let body = config_response_body(get_config(State(state.clone())).await.into_response()).await;
    let entry = &body["gitPlatforms"][0];
    assert_eq!(entry["token"], "");
    assert_eq!(entry["webhookSecret"], "");
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("glpat-platform"), "secret leaked in {serialized}");
    assert!(!serialized.contains("wh-platform"), "secret leaked in {serialized}");

    // Explicitly re-entered secrets on the new baseUrl save normally.
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({
            "gitPlatforms": [{
                "name": "testbed",
                "type": "gitlab",
                "baseUrl": "http://gitlab.internal:9000",
                "token": "glpat-other-instance"
            }]
        })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        stored_platform(&state, "testbed").unwrap().token,
        "glpat-other-instance"
    );
}

/// baseUrl must be an absolute http(s) URL with a host: empty, unparseable,
/// or non-http(s) values fail the whole PUT with 422 and nothing persists.
#[tokio::test]
async fn put_git_platforms_invalid_base_url_rejected() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    let state = state_with_openai("sk-primary");

    for base_url in ["", "   ", "not a url", "ftp://gitlab.internal", "http:///no-host"] {
        let mut bad = testbed_platform_json();
        bad["baseUrl"] = serde_json::json!(base_url);
        let resp = put_config(State(state.clone()), Json(serde_json::json!({ "gitPlatforms": [bad] })))
            .await
            .into_response();
        assert_eq!(
            resp.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "baseUrl {base_url:?} must be rejected"
        );
    }
    assert!(
        state.git_platforms.read().unwrap().is_empty(),
        "rejected updates must not persist"
    );
}

/// `internalBaseUrl` round-trips: PUT stores it on the model, GET projects it
/// back in camelCase, and it is never masked (it is not a secret).
#[tokio::test]
async fn put_get_git_platforms_internal_base_url_round_trip() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    let state = state_with_openai("sk-primary");

    let mut p = testbed_platform_json();
    p["internalBaseUrl"] = serde_json::json!("https://gitlab.islet.space");
    let resp = put_config(State(state.clone()), Json(serde_json::json!({ "gitPlatforms": [p] })))
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    let stored = stored_platform(&state, "testbed").expect("platform must be stored");
    assert_eq!(stored.internal_base_url, "https://gitlab.islet.space");
    assert_eq!(stored.base_url, "http://gitlab.internal:8929");

    let body = config_response_body(get_config(State(state.clone())).await.into_response()).await;
    let entry = &body["gitPlatforms"][0];
    assert_eq!(entry["internalBaseUrl"], "https://gitlab.islet.space");
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(
        serialized.contains("internalBaseUrl"),
        "camelCase field missing: {serialized}"
    );
}

/// `internalBaseUrl` is optional and validated like baseUrl when non-empty:
/// empty / whitespace-only (treated as empty = unconfigured) is accepted and
/// stored empty; a non-absolute / non-http(s) / authority-less value fails the
/// whole PUT with 422 and nothing persists.
#[tokio::test]
async fn put_git_platforms_internal_base_url_validation() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    let state = state_with_openai("sk-primary");

    // Empty and whitespace-only are accepted → stored as unconfigured.
    for internal in ["", "   "] {
        let mut p = testbed_platform_json();
        p["internalBaseUrl"] = serde_json::json!(internal);
        let resp = put_config(State(state.clone()), Json(serde_json::json!({ "gitPlatforms": [p] })))
            .await
            .into_response();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "internalBaseUrl {internal:?} must be allowed"
        );
        assert!(
            stored_platform(&state, "testbed").unwrap().internal_base_url.is_empty(),
            "empty/whitespace internalBaseUrl stores as unconfigured"
        );
    }

    // Non-absolute / non-http(s) / authority-less values → 422.
    for internal in ["not a url", "ftp://gitlab.internal", "http:///no-host"] {
        let mut bad = testbed_platform_json();
        bad["internalBaseUrl"] = serde_json::json!(internal);
        let resp = put_config(State(state.clone()), Json(serde_json::json!({ "gitPlatforms": [bad] })))
            .await
            .into_response();
        assert_eq!(
            resp.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "internalBaseUrl {internal:?} must be rejected"
        );
    }
    // The accepted (empty) value survives; the rejected ones never landed.
    let stored = stored_platform(&state, "testbed").unwrap();
    assert!(stored.internal_base_url.is_empty());
}

/// Full-replace semantics: the submitted array replaces the whole set — an
/// entry absent from the array is removed (the deletion mechanism).
#[tokio::test]
async fn put_git_platforms_full_replace_removes_absent_entries() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    let state = state_with_openai("sk-primary");

    let mut second = testbed_platform_json();
    second["name"] = serde_json::json!("prod");
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({ "gitPlatforms": [testbed_platform_json(), second] })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(state.git_platforms.read().unwrap().len(), 2);

    // Re-submit with only one entry → the other is gone.
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({ "gitPlatforms": [testbed_platform_json()] })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        stored_platform(&state, "prod").is_none(),
        "absent entry must be removed"
    );
    assert!(stored_platform(&state, "testbed").is_some());

    // An explicit empty array clears the set.
    let resp = put_config(State(state.clone()), Json(serde_json::json!({ "gitPlatforms": [] })))
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(state.git_platforms.read().unwrap().is_empty());
}

/// A sparse PUT that omits `gitPlatforms` leaves the configured set alone
/// (partial-update semantics, same as every other section).
#[tokio::test]
async fn put_config_sparse_patch_preserves_git_platforms() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    let state = state_with_openai("sk-primary");
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({ "gitPlatforms": [testbed_platform_json()] })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({ "rules": { "minScore": 90 } })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let stored = stored_platform(&state, "testbed").expect("omitted gitPlatforms must survive a sparse PUT");
    assert_eq!(stored.token, "glpat-platform", "secrets survive the masked round trip");
    assert_eq!(state.ui_config.read().unwrap().git_platforms[0].token, API_KEY_MASK);
}

/// Only `gitlab` is implemented: another `type` fails the whole PUT with
/// 422, and nothing is persisted.
#[tokio::test]
async fn put_git_platforms_unknown_type_rejected() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    let state = state_with_openai("sk-primary");
    let mut bad = testbed_platform_json();
    bad["type"] = serde_json::json!("gitea");
    let resp = put_config(State(state.clone()), Json(serde_json::json!({ "gitPlatforms": [bad] })))
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        state.git_platforms.read().unwrap().is_empty(),
        "rejected update must not persist"
    );
}

/// Entries with a blank name are skipped; a duplicate name keeps the last
/// occurrence.
#[tokio::test]
async fn put_git_platforms_skips_nameless_and_dedupes() {
    let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    let state = state_with_openai("sk-primary");
    let resp = put_config(
        State(state.clone()),
        Json(serde_json::json!({
            "gitPlatforms": [
                { "name": "", "type": "gitlab", "baseUrl": "http://x.internal" },
                { "name": "testbed", "type": "gitlab", "baseUrl": "http://a.internal", "token": "glpat-a" },
                { "name": "testbed", "type": "gitlab", "baseUrl": "http://b.internal", "token": "glpat-b" }
            ]
        })),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), StatusCode::OK);
    let platforms = state.git_platforms.read().unwrap();
    assert_eq!(platforms.len(), 1);
    assert_eq!(
        platforms[0].base_url, "http://b.internal",
        "duplicate name: last write wins"
    );
    assert_eq!(platforms[0].token, "glpat-b");
}

// ── POST /config/git-platforms/test probe ─────────────────────────

async fn probe_git_platform(state: Arc<AppState>, base_url: &str, token: &str) -> serde_json::Value {
    let req: super::helpers::TestGitPlatformRequest =
        serde_json::from_value(serde_json::json!({ "baseUrl": base_url, "token": token }))
            .expect("TestGitPlatformRequest must deserialize");
    let resp = super::helpers::test_git_platform(State(state), Json(req))
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::OK, "probe errors stay in the body");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn git_platform_probe_reports_version_on_success() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/version"))
        .and(header("Authorization", "Bearer glpat-explicit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "version": "19.2.4-ee",
            "revision": "abc123"
        })))
        .mount(&server)
        .await;

    let body = probe_git_platform(Arc::new(AppState::new(vec![])), &server.uri(), "glpat-explicit").await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["version"], "19.2.4-ee");
}

/// The masked-token fallback: a blank/masked probe token resolves to the
/// stored token of the platform with the same baseUrl (fetch_models pattern).
#[tokio::test]
async fn git_platform_probe_falls_back_to_stored_token_when_masked() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/version"))
        .and(header("Authorization", "Bearer glpat-stored"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "version": "19.2.4-ee" })))
        .mount(&server)
        .await;

    let state = Arc::new(AppState::new(vec![]));
    *state.git_platforms.write().unwrap() = vec![crate::models::GitPlatformConfig {
        name: "testbed".to_string(),
        platform_type: "gitlab".to_string(),
        base_url: server.uri(),
        internal_base_url: String::new(),
        token: "glpat-stored".to_string(),
        webhook_secret: String::new(),
        webhook_signing_secret: String::new(),
        allowed_projects: Vec::new(),
    }];

    for token in [API_KEY_MASK, ""] {
        let body = probe_git_platform(state.clone(), &server.uri(), token).await;
        assert_eq!(
            body["ok"], true,
            "token {token:?} must fall back to the stored one: {body}"
        );
        assert_eq!(body["version"], "19.2.4-ee");
    }
}

/// Probe failures surface in the body: HTTP errors as `HTTP <status>`, and
/// a masked token with no matching platform is sent verbatim (unchanged old
/// behavior — the upstream 401 becomes the error).
#[tokio::test]
async fn git_platform_probe_reports_http_errors() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/version"))
        .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
        .mount(&server)
        .await;

    let body = probe_git_platform(Arc::new(AppState::new(vec![])), &server.uri(), API_KEY_MASK).await;
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"], "HTTP 401 Unauthorized");
}

#[tokio::test]
async fn git_platform_probe_validates_base_url() {
    let state = Arc::new(AppState::new(vec![]));
    let body = probe_git_platform(state.clone(), "", "tok").await;
    assert_eq!(body["ok"], false);
    assert!(body["error"].as_str().unwrap().contains("baseUrl"), "got {body}");
    let body = probe_git_platform(state, "not a url", "tok").await;
    assert_eq!(body["ok"], false);
    assert!(
        body["error"].as_str().unwrap().contains("invalid baseUrl"),
        "got {body}"
    );
}

/// SSRF: the probe applies the same address policy as review webhook
/// callbacks — link-local/metadata targets are blocked under both schemes,
/// and plain http is only allowed for loopback/private targets. All of
/// these fail validation BEFORE any network request, so no mock is needed.
#[tokio::test]
async fn git_platform_probe_blocks_ssrf_targets() {
    let state = Arc::new(AppState::new(vec![]));

    // Cloud metadata endpoint — blocked even over https.
    let body = probe_git_platform(state.clone(), "https://169.254.169.254/latest/meta-data", "tok").await;
    assert_eq!(body["ok"], false);
    assert!(
        body["error"].as_str().unwrap().contains("blocked range"),
        "metadata target must be blocked: {body}"
    );
    let body = probe_git_platform(state.clone(), "http://169.254.169.254/", "tok").await;
    assert_eq!(body["ok"], false);
    assert!(
        body["error"].as_str().unwrap().contains("invalid baseUrl"),
        "got {body}"
    );

    // Unspecified address — blocked.
    let body = probe_git_platform(state.clone(), "http://0.0.0.0:9000/", "tok").await;
    assert_eq!(body["ok"], false);
    assert!(
        body["error"].as_str().unwrap().contains("blocked range"),
        "0.0.0.0 must be blocked: {body}"
    );

    // http to a PUBLIC host is rejected (https would be required).
    let body = probe_git_platform(state.clone(), "http://93.184.216.34/", "tok").await;
    assert_eq!(body["ok"], false);
    assert!(
        body["error"].as_str().unwrap().contains("loopback/private"),
        "public http must be rejected: {body}"
    );
}

/// The guard must not be stricter than the review webhook policy: loopback
/// and private-network instances (the primary deployment case, e.g. the
/// E2E testbed at http://localhost:8929) still probe. Port 9 is the
/// discard port — validation passes and the request itself fails with a
/// connect error, proving the target was not rejected by the SSRF guard.
#[tokio::test]
async fn git_platform_probe_allows_loopback_and_private_targets() {
    let state = Arc::new(AppState::new(vec![]));
    for base in ["http://localhost:9", "http://127.0.0.1:9", "http://10.255.255.1:9"] {
        let body = probe_git_platform(state.clone(), base, "tok").await;
        assert_eq!(body["ok"], false, "unreachable target fails the probe: {body}");
        assert!(
            !body["error"].as_str().unwrap().contains("invalid baseUrl"),
            "{base} must pass SSRF validation and fail at connect time instead: {body}"
        );
    }
}
