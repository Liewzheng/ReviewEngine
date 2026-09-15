//! REST API endpoints for LLM provider management.
//!
//! Lists configured providers, tests connectivity, and provides
//! CRUD operations for multi-provider management.

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use std::sync::Arc;

use super::llm_usage::{UsageSnapshot, USAGE_WINDOW_DAYS};
use crate::server::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/providers", get(get_providers).post(add_provider))
        .route("/providers/{id}/test", post(test_provider))
        .route("/providers/{id}", delete(delete_provider).put(update_provider))
}

async fn get_providers(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    // Read the primary BEFORE taking the provider lock and never nest the two
    // guards (`PUT /config` holds `llm_configs` and `ui_config` in that order).
    let primary = state.ui_config.read().unwrap().llm.primary_provider.clone();
    // Clone out of the lock: the health report probes (`await`) and no std
    // guard may be held across an await point.
    let configs: Vec<crate::models::LLMConfig> = state.llm_configs.read().unwrap().clone();
    // RENG-36: the status comes from the probe cache, so a provider whose
    // credentials were just changed reports the probe of the NEW config (a
    // missing entry is probed before this returns) instead of a stale
    // "healthy" left over from the old key.
    let health = state.llm_health.report(&configs).await;
    // RENG-56: what the providers actually served, from the recorded reviews.
    // One aggregate read per request; `None` (no store / query failed) makes
    // every usage metric `null` rather than a fabricated zero.
    let usage_now = chrono::Utc::now();
    let usage = super::llm_usage::snapshot(&state, usage_now).await;
    Json(serde_json::json!({
        // The numbers below are meaningless without their window, so the
        // window travels with them (rolling `USAGE_WINDOW_DAYS` days).
        "usageWindowDays": USAGE_WINDOW_DAYS,
        "usageSince": super::llm_usage::window_start(usage_now).to_rfc3339(),
        "usageAvailable": usage.is_some(),
        // Every usage recorded in the window, across ALL provider names —
        // including ones that are no longer configured, which is why this can
        // exceed the sum of the cards. It is the denominator of every
        // `usageShare`, so the page can state it instead of guessing.
        "usageTotal": usage.as_ref().map(|snapshot| snapshot.total_usage),
        "items": provider_items(&primary, &configs, &health, usage.as_ref()),
    }))
}

/// The `GET /llm/providers` card payload for one stored provider list.
///
/// `position` is the index in the STORED list (the value
/// `llm_providers.raw.position` persists, i.e. what the UI array order
/// encodes); `chainPosition` is the 1-based rank in the authoritative runtime
/// chain ([`crate::llm::chain_positions`], the primary leading) and
/// `isPrimary` marks its head — so the LLM page can show what a review will
/// actually use (RENG-55).
///
/// `health` holds one report per config, in the same order
/// ([`AppState::llm_health`](crate::server::AppState::llm_health)); `status`
/// and `latencyMs` come from it, never from a guess about the config (RENG-36).
///
/// `usage` holds the recorded usage of the window (RENG-56). Every usage
/// metric is `null` when the window cannot be read (`None`) or holds nothing
/// to derive it from — the page renders `—`; only a count that was really
/// measured may be `0`.
fn provider_items(
    primary: &str,
    configs: &[crate::models::LLMConfig],
    health: &[super::llm_health::ProviderHealth],
    usage: Option<&UsageSnapshot>,
) -> Vec<serde_json::Value> {
    let ranks = crate::llm::chain_positions(primary, configs);
    configs
        .iter()
        .enumerate()
        .map(|(i, cfg)| {
            let id = format!("{}-{}", cfg.provider, i);
            let chain_position = ranks.get(i).copied().unwrap_or(i + 1);
            let report = health.get(i);
            let usage = usage.map(|snapshot| snapshot.for_provider(&cfg.provider));
            serde_json::json!({
                "id": id,
                "name": cfg.provider,
                "logo": logo_for_provider(&cfg.provider),
                "status": report
                    .map(|h| h.status.as_str())
                    .unwrap_or_else(|| super::llm_health::ProviderStatus::Offline.as_str()),
                "configured": !cfg.api_key.is_empty(),
                // Echo the editable config back so the UI can prefill the edit
                // form. The API key is intentionally never returned.
                "apiBaseUrl": cfg.api_base,
                "defaultModel": cfg.model,
                "maxTokens": cfg.max_tokens,
                "temperature": round_temperature(cfg.temperature),
                "position": i,
                "chainPosition": chain_position,
                "isPrimary": chain_position == 1,
                // Round-trip time of the probe behind `status` (0 when the
                // provider was not probed because it has no key).
                "latencyMs": report.map(|h| h.latency_ms).unwrap_or(0),
                // RENG-56: recorded usage of the window (see the module docs of
                // `llm_usage`). `requestCount` counts REVIEWS that used the
                // provider (review-level granularity, cross-checkable against
                // `GET /api/v1/reviews`); `usageShare` is its share of all
                // recorded usage in the window; `successRate` is over the
                // reviews that used it and reached an outcome.
                "requestCount": usage.as_ref().and_then(|u| u.request_count),
                "usageShare": usage.as_ref().and_then(|u| u.usage_share),
                "successRate": usage.as_ref().and_then(|u| u.success_rate),
                "lastUsedAt": usage
                    .as_ref()
                    .and_then(|u| u.last_used_at)
                    .map(|t| t.to_rfc3339()),
                // Timestamp of the probe behind `status`; `null` when no probe
                // happened — for an `offline` provider (no key, never probed)
                // as much as for a config with no report at all. Never "now",
                // which would claim a check that did not happen.
                "lastChecked": report
                    .filter(|h| h.status != super::llm_health::ProviderStatus::Offline)
                    .map(|h| h.checked_at.to_rfc3339()),
            })
        })
        .collect()
}

// ─── Add Provider ─────────────────────────────────────────────────

/// Request body for adding a new provider.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddProviderRequest {
    pub provider: String,
    #[serde(default, alias = "defaultModel")]
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default, alias = "apiBaseUrl")]
    pub api_base: String,
    #[serde(default = "default_add_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_add_temperature")]
    pub temperature: f32,
}

fn default_add_max_tokens() -> u32 {
    4096
}
fn default_add_temperature() -> f32 {
    0.7
}

async fn add_provider(
    State(state): State<Arc<AppState>>,
    body: Result<Json<AddProviderRequest>, JsonRejection>,
) -> impl IntoResponse {
    let Json(body) = match body {
        Ok(json) => json,
        Err(rejection) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": rejection.body_text() })),
            )
                .into_response();
        }
    };

    // Validate required fields
    if body.provider.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "provider name is required" })),
        )
            .into_response();
    }
    if body.api_key.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "api_key is required" })),
        )
            .into_response();
    }

    let new_cfg = crate::models::LLMConfig {
        provider: body.provider.clone(),
        model: body.model.clone(),
        api_key: body.api_key.clone(),
        api_base: body.api_base.clone(),
        max_tokens: body.max_tokens,
        temperature: body.temperature,
        disable_thinking: None,
    };

    // Derive the new id for the response
    let idx = {
        let guard = state.llm_configs.read().unwrap();
        guard.len()
    };
    let id = format!("{}-{}", body.provider, idx);

    // Add to state.llm_configs
    {
        let mut guard = state.llm_configs.write().unwrap();
        guard.push(new_cfg.clone());
    }
    // RENG-36: the health cache follows the effective provider set — the new
    // provider has no cached entry, so its first read probes it.
    invalidate_removed_health(&state);

    // Sync with state.app_config if present
    {
        let mut cfg_opt = state.app_config.write().unwrap();
        if let Some(arc) = cfg_opt.as_ref() {
            let mut new_cfg_app = (**arc).clone();
            new_cfg_app.llm.push(new_cfg);
            *cfg_opt = Some(Arc::new(new_cfg_app));
        }
    }

    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": id,
            "provider": body.provider,
            "model": body.model,
            "configured": true,
        })),
    )
        .into_response()
}

// ─── Delete Provider ──────────────────────────────────────────────

async fn delete_provider(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> impl IntoResponse {
    // Parse id: expects format "{provider}-{idx}"
    let (provider, idx_str) = match id.rsplit_once('-') {
        Some((p, i)) => (p.to_string(), i.to_string()),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid provider id format" })),
            )
                .into_response();
        }
    };

    let idx: usize = match idx_str.parse() {
        Ok(i) => i,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid provider id index" })),
            )
                .into_response();
        }
    };

    let removed = {
        let mut guard = state.llm_configs.write().unwrap();
        if idx >= guard.len() {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Provider not found" })),
            )
                .into_response();
        }
        // Verify the provider at this index matches the expected provider name
        let actual_provider = &guard[idx].provider;
        if *actual_provider != provider {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Provider id mismatch" })),
            )
                .into_response();
        }
        let removed_cfg = guard.remove(idx);
        // Rebuild: keep providers contiguous — the id scheme {provider}-{i}
        // relies on index, so we keep the list as-is after removal (indices
        // shift for subsequent entries, but that's acceptable).
        removed_cfg
    };
    // RENG-36: the removed provider's cached health goes with it, so the list
    // never reports a provider that is no longer configured.
    invalidate_removed_health(&state);

    // Sync with state.app_config if present
    {
        let mut cfg_opt = state.app_config.write().unwrap();
        if let Some(arc) = cfg_opt.as_ref() {
            let mut new_cfg = (**arc).clone();
            new_cfg
                .llm
                .retain(|c| c.provider != removed.provider || c.api_key != removed.api_key);
            *cfg_opt = Some(Arc::new(new_cfg));
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({ "status": "deleted", "id": id })),
    )
        .into_response()
}

// ─── Update Provider ──────────────────────────────────────────────

/// Request body for updating an existing provider.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProviderRequest {
    #[serde(default, alias = "defaultModel")]
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default, alias = "apiBaseUrl")]
    pub api_base: String,
    #[serde(default = "default_add_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_add_temperature")]
    pub temperature: f32,
}

async fn update_provider(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Result<Json<UpdateProviderRequest>, JsonRejection>,
) -> impl IntoResponse {
    let Json(body) = match body {
        Ok(json) => json,
        Err(rejection) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": rejection.body_text() })),
            )
                .into_response();
        }
    };

    let (provider, idx_str) = match id.rsplit_once('-') {
        Some((p, i)) => (p.to_string(), i.to_string()),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid provider id format" })),
            )
                .into_response();
        }
    };

    let idx: usize = match idx_str.parse() {
        Ok(i) => i,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid provider id index" })),
            )
                .into_response();
        }
    };

    let updated = {
        let mut guard = state.llm_configs.write().unwrap();
        if idx >= guard.len() {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Provider not found" })),
            )
                .into_response();
        }
        let actual_provider = &guard[idx].provider;
        if *actual_provider != provider {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Provider id mismatch" })),
            )
                .into_response();
        }

        let cfg = &mut guard[idx];
        if !body.model.is_empty() {
            cfg.model = body.model.clone();
        }
        if !body.api_key.is_empty() {
            cfg.api_key = body.api_key.clone();
        }
        if !body.api_base.is_empty() {
            cfg.api_base = body.api_base.clone();
        }
        cfg.max_tokens = body.max_tokens;
        cfg.temperature = body.temperature;
        cfg.clone()
    };
    // RENG-36: the edited config's cached health is dropped (its fingerprint is
    // no longer in the set), so the next read probes the new credentials.
    invalidate_removed_health(&state);

    // Sync with state.app_config if present
    {
        let mut cfg_opt = state.app_config.write().unwrap();
        if let Some(arc) = cfg_opt.as_ref() {
            let mut new_cfg = (**arc).clone();
            if idx < new_cfg.llm.len() {
                new_cfg.llm[idx] = updated.clone();
            }
            *cfg_opt = Some(Arc::new(new_cfg));
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "updated",
            "id": id,
            "provider": provider,
            "model": updated.model,
        })),
    )
        .into_response()
}

// ─── Test Provider ────────────────────────────────────────────────

async fn test_provider(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Json<serde_json::Value> {
    let cfg = {
        let guard = state.llm_configs.read().unwrap();
        guard
            .iter()
            .enumerate()
            .find(|(i, c)| format!("{}-{}", c.provider, i) == id)
            .map(|(_, c)| c.clone())
    };

    let cfg = match cfg {
        Some(c) => c,
        None => {
            return Json(serde_json::json!({
                "success": false,
                "error": "Provider not found",
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }));
        }
    };

    if cfg.api_key.is_empty() {
        return Json(serde_json::json!({
            "success": false,
            "error": "Missing API key",
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }));
    }

    let start = std::time::Instant::now();
    let result = crate::llm::probe::probe_llm_connectivity(&cfg).await;
    let latency_ms = start.elapsed().as_millis() as u64;

    let (success, error, resolved_base) = match result {
        Ok(outcome) => (true, None::<String>, Some(outcome.resolved_base)),
        Err(e) => (
            false,
            Some(e.to_string()),
            // The probe may have failed fast during resolution (unknown
            // provider, empty api_base) — recover the URL when possible so
            // the UI can show where the key would have gone.
            crate::llm::probe::resolve_api_base(&cfg).ok(),
        ),
    };

    // The user just asked for this provider's connectivity — record it as the
    // provider's health so `GET /llm/providers` (and the dashboard) report what
    // the Test Connection button reported, instead of re-probing (RENG-36).
    state.llm_health.record(
        &cfg,
        match &error {
            None => super::llm_health::ProviderHealth::healthy(latency_ms),
            Some(message) => super::llm_health::ProviderHealth::error(message.clone(), latency_ms),
        },
    );

    Json(serde_json::json!({
        "success": success,
        "latencyMs": latency_ms,
        "error": error,
        "resolvedApiBase": resolved_base,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    }))
}

/// Drop the cached health of every provider that is no longer in
/// `state.llm_configs` (RENG-36). Called by the provider CRUD handlers after
/// they mutate the list; `PUT /api/v1/config` does the same inside
/// `apply_ui_config`, so every path that changes credentials or the provider
/// set goes through one invalidation rule: an entry survives only while its
/// exact config is still configured.
fn invalidate_removed_health(state: &Arc<AppState>) {
    let live = state.llm_configs.read().unwrap();
    state.llm_health.retain_live(&live);
}

fn logo_for_provider(provider: &str) -> String {
    match provider.to_lowercase().as_str() {
        "openai" => "OpenAI",
        "anthropic" => "Anthropic",
        "ollama" => "Ollama",
        "azure" => "Azure",
        "google" => "Google",
        "cohere" => "Cohere",
        _ => "Generic",
    }
    .to_string()
}

/// Serialize an `f32` temperature at 2-decimal precision so the JSON output is
/// `0.3` instead of the raw f32 noise (`0.30000001192092896`).
fn round_temperature(t: f32) -> f64 {
    ((t as f64) * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::api::config::{put_config, UiConfig};
    use crate::server::api::llm_health::{ProviderHealth, ProviderStatus};
    use crate::store::traits::ProviderUsageStats;

    /// Unit 12: temperature serializes at 2-decimal precision, free of f32 noise.
    #[test]
    fn round_temperature_removes_f32_noise() {
        assert_eq!(round_temperature(0.3), 0.3);
        assert_eq!(round_temperature(0.7), 0.7);
        assert_eq!(round_temperature(1.0), 1.0);
        assert_eq!(round_temperature(0.0), 0.0);
        // Not the raw f32 value 0.30000001192092896.
        assert_eq!(serde_json::to_string(&round_temperature(0.3)).unwrap(), "0.3");
    }

    fn cfg(provider: &str) -> crate::models::LLMConfig {
        crate::models::LLMConfig {
            provider: provider.to_string(),
            model: format!("{provider}-model"),
            api_key: "k".to_string(),
            api_base: format!("https://api.{provider}.example/v1"),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
        }
    }

    /// Health reports matching `configs` (the shape `AppState::llm_health`
    /// produces), used by the payload tests that are not about probing.
    fn healthy(n: usize) -> Vec<ProviderHealth> {
        (0..n).map(|_| ProviderHealth::healthy(7)).collect()
    }

    /// RENG-55: the card payload exposes the chain, not just the stored list —
    /// `position` stays the stored index, `chainPosition`/`isPrimary` describe
    /// the runtime order the page renders.
    #[test]
    fn provider_items_expose_chain_position_and_primary() {
        let stored = vec![cfg("xiaomi"), cfg("deepseek")];
        let items = provider_items("deepseek", &stored, &healthy(stored.len()), None);

        // Stored order (and the `{provider}-{index}` ids) is untouched.
        assert_eq!(items[0]["name"], "xiaomi");
        assert_eq!(items[0]["id"], "xiaomi-0");
        assert_eq!(items[0]["position"], 0);
        assert_eq!(items[1]["name"], "deepseek");
        assert_eq!(items[1]["position"], 1);

        // Chain: deepseek leads.
        assert_eq!(items[1]["chainPosition"], 1);
        assert_eq!(items[1]["isPrimary"], true);
        assert_eq!(items[0]["chainPosition"], 2);
        assert_eq!(items[0]["isPrimary"], false);
    }

    /// Without a usable primary selection the stored order is authoritative:
    /// the head is the effective primary (never a blank card).
    #[test]
    fn provider_items_head_is_primary_without_a_primary_selection() {
        let stored = vec![cfg("xiaomi"), cfg("deepseek")];
        let items = provider_items("", &stored, &healthy(stored.len()), None);
        assert_eq!(items[0]["isPrimary"], true);
        assert_eq!(items[1]["isPrimary"], false);
        assert_eq!(items[1]["chainPosition"], 2);
        // A primary naming a provider the runtime no longer holds (stale
        // `primaryProvider` in the echo) degrades to the same rule.
        let items = provider_items("ghost", &[cfg("xiaomi")], &healthy(1), None);
        assert_eq!(items[0]["isPrimary"], true);
        assert_eq!(items[0]["chainPosition"], 1);
    }

    /// Empty provider set → empty payload, no panic.
    #[test]
    fn provider_items_empty_set() {
        assert!(provider_items("deepseek", &[], &[], None).is_empty());
    }

    /// RENG-36: the card's `status`/`latencyMs`/`lastChecked` come from the
    /// probe report — never from a guess about the config's shape. A provider
    /// with a key can therefore be `error`, which is the whole point: it used
    /// to be reported `healthy` for no other reason than having a key.
    #[test]
    fn provider_items_report_probed_status_not_key_presence() {
        let stored = vec![cfg("xiaomi"), cfg("deepseek")];
        let items = provider_items(
            "xiaomi",
            &stored,
            &[
                ProviderHealth::error("HTTP 401 Unauthorized", 12),
                ProviderHealth::healthy(34),
            ],
            None,
        );

        assert_eq!(items[0]["status"], "error");
        assert_eq!(items[0]["configured"], true, "a key is still stored");
        assert_eq!(items[0]["latencyMs"], 12);
        assert_eq!(items[1]["status"], "healthy");
        assert_eq!(items[1]["latencyMs"], 34);
        // `lastChecked` is the probe's timestamp, i.e. a real check time.
        for item in &items {
            assert!(
                chrono::DateTime::parse_from_rfc3339(item["lastChecked"].as_str().unwrap()).is_ok(),
                "lastChecked must be RFC3339: {item}"
            );
        }
        // A provider with no stored key is `offline` and never probed.
        let mut blank = cfg("xiaomi");
        blank.api_key = String::new();
        let items = provider_items("xiaomi", &[blank], &[ProviderHealth::offline()], None);
        assert_eq!(items[0]["status"], "offline");
        assert_eq!(items[0]["configured"], false);
        assert_eq!(items[0]["latencyMs"], 0);
        // RENG-56: a provider that was never probed has no check time — the
        // payload says `null` instead of stamping the current time.
        assert_eq!(items[0]["lastChecked"], serde_json::Value::Null);
        // A config with no report at all (misaligned health list) is the same.
        let items = provider_items("xiaomi", &stored, &[], None);
        assert_eq!(items[0]["lastChecked"], serde_json::Value::Null);
        assert_eq!(ProviderStatus::Offline.dashboard_str(), "offline");
        assert_eq!(ProviderStatus::Error.dashboard_str(), "error");
        assert_eq!(ProviderStatus::Healthy.dashboard_str(), "success");
    }

    // ─── RENG-56: recorded usage statistics ─────────────────────────

    /// The removed fabricated fields are gone from the card payload: there is
    /// no capacity concept and no time series, so `usagePercent` / `sparkline`
    /// (and the hardcoded `errorRate`) must not be echoed to the client.
    #[test]
    fn provider_items_omit_the_fabricated_metric_fields() {
        let stored = vec![cfg("xiaomi")];
        let snapshot = UsageSnapshot::new(
            chrono::Utc::now(),
            vec![ProviderUsageStats {
                provider: "xiaomi".to_string(),
                usage_count: 4,
                completed_count: 4,
                failed_count: 0,
                last_used_at: Some(chrono::Utc::now()),
            }],
        );
        let items = provider_items("xiaomi", &stored, &healthy(1), Some(&snapshot));
        let item = items[0].as_object().unwrap();

        for gone in ["usagePercent", "sparkline", "errorRate"] {
            assert!(!item.contains_key(gone), "{gone} must not be in the payload: {item:?}");
        }
        for present in ["requestCount", "usageShare", "successRate", "lastUsedAt"] {
            assert!(item.contains_key(present), "{present} missing from {item:?}");
        }
    }

    /// Without a readable store (`REVIEW_DISABLE_DB`, aggregate failure) every
    /// usage metric is `null`: unknown, not zero.
    #[test]
    fn provider_items_without_a_store_report_null_usage() {
        let stored = vec![cfg("xiaomi")];
        let items = provider_items("xiaomi", &stored, &healthy(1), None);
        for field in ["requestCount", "usageShare", "successRate", "lastUsedAt"] {
            assert_eq!(items[0][field], serde_json::Value::Null, "{field} must be null");
        }
    }

    /// With a store that recorded nothing, the count is a measured zero and
    /// everything derived from a nonexistent denominator is `null`.
    #[test]
    fn provider_items_empty_window_reports_zero_count_and_null_derivations() {
        let stored = vec![cfg("xiaomi")];
        let snapshot = UsageSnapshot::new(chrono::Utc::now(), Vec::new());
        let items = provider_items("xiaomi", &stored, &healthy(1), Some(&snapshot));

        assert_eq!(items[0]["requestCount"], 0);
        assert_eq!(items[0]["usageShare"], serde_json::Value::Null);
        assert_eq!(items[0]["successRate"], serde_json::Value::Null);
        assert_eq!(items[0]["lastUsedAt"], serde_json::Value::Null);
    }

    /// The card carries the recorded numbers, not placeholders.
    #[test]
    fn provider_items_report_recorded_usage() {
        let stored = vec![cfg("xiaomi"), cfg("deepseek")];
        let last_used = chrono::DateTime::parse_from_rfc3339("2026-09-14T08:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let snapshot = UsageSnapshot::new(
            chrono::Utc::now(),
            vec![
                ProviderUsageStats {
                    provider: "xiaomi".to_string(),
                    usage_count: 3,
                    completed_count: 3,
                    failed_count: 0,
                    last_used_at: Some(last_used),
                },
                ProviderUsageStats {
                    provider: "deepseek".to_string(),
                    usage_count: 1,
                    completed_count: 0,
                    failed_count: 1,
                    last_used_at: None,
                },
            ],
        );
        let items = provider_items("xiaomi", &stored, &healthy(2), Some(&snapshot));

        assert_eq!(items[0]["requestCount"], 3);
        assert_eq!(items[0]["usageShare"], 0.75);
        assert_eq!(items[0]["successRate"], 1.0);
        assert_eq!(items[0]["lastUsedAt"], "2026-09-14T08:30:00+00:00");

        assert_eq!(items[1]["requestCount"], 1);
        assert_eq!(items[1]["usageShare"], 0.25);
        assert_eq!(items[1]["successRate"], 0.0, "the only attributed outcome failed");
        // A provider whose rows carried no decodable timestamp has no
        // `lastUsedAt` — not a blank string, not "now".
        assert_eq!(items[1]["lastUsedAt"], serde_json::Value::Null);
    }

    // ─── RENG-56: usage end to end (store → handler payload) ────────

    /// One `reviews` row carrying an LLM summary, as the review path writes it.
    async fn seed_review(
        store: &crate::store::SqlxStore,
        state: crate::server::task_queue::TaskState,
        created_at: chrono::DateTime<chrono::Utc>,
        summary: Option<String>,
    ) {
        use crate::store::traits::ReviewStore;
        let entry = crate::server::task_queue::TaskEntry {
            task_id: uuid::Uuid::new_v4(),
            state,
            created_at,
            started_at: None,
            completed_at: None,
            result: None,
            error: None,
            request: None,
            source_meta: Default::default(),
            progress: None,
            expert_name: None,
            llm_summary: summary,
        };
        ReviewStore::create(store, &entry).await.unwrap();
    }

    /// A state whose health probes are stubbed, so the payload tests never
    /// touch the network. `db` is attached by the caller.
    fn stub_state(configs: Vec<crate::models::LLMConfig>) -> Arc<AppState> {
        let mut seeded = AppState::new(configs);
        seeded.llm_health = Arc::new(super::super::llm_health::LlmHealthStore::with_probe(
            Arc::new(|_cfg| {
                Box::pin(async {
                    Ok(crate::llm::probe::ProbeOutcome {
                        resolved_base: "stub".to_string(),
                    })
                })
            }),
            std::time::Duration::from_secs(60),
        ));
        Arc::new(seeded)
    }

    /// The whole path the page reads: recorded reviews in the store become the
    /// card's numbers, with the window travelling alongside them. The counts
    /// are the ones a user can reproduce from `GET /api/v1/reviews`.
    #[tokio::test]
    async fn get_providers_reports_recorded_usage_from_the_store() {
        let store = Arc::new(crate::store::SqlxStore::new_in_memory().await.unwrap());
        store.migrate().await.unwrap();
        let now = chrono::Utc::now();

        // 3 reviews used xiaomi (2 completed, 1 failed) and 1 used deepseek.
        for (state, ago_hours, provider) in [
            (crate::server::task_queue::TaskState::Completed, 1, "xiaomi"),
            (crate::server::task_queue::TaskState::Completed, 2, "xiaomi"),
            (crate::server::task_queue::TaskState::Failed, 3, "xiaomi"),
            (crate::server::task_queue::TaskState::Completed, 4, "deepseek"),
        ] {
            seed_review(
                &store,
                state,
                now - chrono::Duration::hours(ago_hours),
                Some(format!(r#"[{{"provider":"{provider}","model":"{provider}-model"}}]"#)),
            )
            .await;
        }
        // Outside the window: never counted.
        seed_review(
            &store,
            crate::server::task_queue::TaskState::Completed,
            now - chrono::Duration::days(30),
            Some(r#"[{"provider":"xiaomi","model":"old"}]"#.to_string()),
        )
        .await;

        let mut state = stub_state(vec![cfg("xiaomi"), cfg("deepseek")]);
        Arc::get_mut(&mut state).unwrap().db = Some(store);
        let payload = get_providers(State(state)).await.0;

        assert_eq!(payload["usageWindowDays"], 7);
        assert_eq!(payload["usageAvailable"], true);
        assert_eq!(
            payload["usageTotal"], 4,
            "the window total is the share denominator (all recorded usages)"
        );
        assert!(
            chrono::DateTime::parse_from_rfc3339(payload["usageSince"].as_str().unwrap()).is_ok(),
            "the window start must travel as RFC3339: {payload}"
        );

        let xiaomi = &payload["items"][0];
        assert_eq!(xiaomi["requestCount"], 3, "the out-of-window review is excluded");
        assert_eq!(xiaomi["usageShare"], 0.75);
        assert_eq!(xiaomi["successRate"], 0.6667, "2 of 3 decided reviews completed");
        let last_used = chrono::DateTime::parse_from_rfc3339(xiaomi["lastUsedAt"].as_str().unwrap()).unwrap();
        assert!(
            (last_used.with_timezone(&chrono::Utc) - (now - chrono::Duration::hours(1)))
                .num_seconds()
                .abs()
                < 2,
            "lastUsedAt must be the newest recorded review: {xiaomi}"
        );

        let deepseek = &payload["items"][1];
        assert_eq!(deepseek["requestCount"], 1);
        assert_eq!(deepseek["usageShare"], 0.25);
        assert_eq!(deepseek["successRate"], 1.0);
    }

    /// A store that holds no reviewed usage reports a measured zero for the
    /// count and `null` for everything derived — the UI's `—`.
    #[tokio::test]
    async fn get_providers_reports_null_derivations_on_an_empty_store() {
        let store = Arc::new(crate::store::SqlxStore::new_in_memory().await.unwrap());
        store.migrate().await.unwrap();

        let mut state = stub_state(vec![cfg("xiaomi")]);
        Arc::get_mut(&mut state).unwrap().db = Some(store);
        let payload = get_providers(State(state)).await.0;

        assert_eq!(payload["usageAvailable"], true);
        assert_eq!(payload["usageTotal"], 0, "a read window with no usage totals zero");
        let item = &payload["items"][0];
        assert_eq!(item["requestCount"], 0);
        assert_eq!(item["usageShare"], serde_json::Value::Null);
        assert_eq!(item["successRate"], serde_json::Value::Null);
        assert_eq!(item["lastUsedAt"], serde_json::Value::Null);
    }

    /// Without a store the usage metrics are `null` and nothing else about the
    /// card changes: the page must keep working with `REVIEW_DISABLE_DB=1`.
    #[tokio::test]
    async fn get_providers_without_a_store_reports_unknown_usage() {
        let state = stub_state(vec![cfg("xiaomi")]);
        let payload = get_providers(State(state)).await.0;

        assert_eq!(payload["usageAvailable"], false);
        assert_eq!(payload["usageTotal"], serde_json::Value::Null);
        let item = &payload["items"][0];
        assert_eq!(item["name"], "xiaomi");
        assert_eq!(item["status"], "healthy");
        for field in ["requestCount", "usageShare", "successRate", "lastUsedAt"] {
            assert_eq!(item[field], serde_json::Value::Null, "{field} must be null");
        }
    }

    // ─── RENG-36: health follows the credentials ────────────────────
    /// The reported sequence, end to end: a provider probed healthy, its key
    /// broken through `PUT /api/v1/config`, the next read no longer `healthy`
    /// (and not a stale leftover), then a good key again → `healthy` again.
    ///
    /// The probe is the REAL `probe_llm_connectivity` (the store's default), so
    /// this pins the reported bug against an actual HTTP 401 rather than a
    /// stubbed verdict: the wiremock `/models` route accepts only
    /// `Bearer sk-good-key` and answers 401 to anything else.
    #[tokio::test]
    async fn credential_change_invalidates_cached_health() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _rt_lock = crate::server::gitlab::RUNTIME_TEST_LOCK.lock().await;
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header("Authorization", "Bearer sk-good-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid api key"))
            .mount(&mock)
            .await;

        let provider = crate::models::LLMConfig {
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            api_key: "sk-good-key".to_string(),
            api_base: mock.uri(),
            max_tokens: 4096,
            temperature: 0.7,
            disable_thinking: None,
        };
        let state = Arc::new(AppState::new(vec![provider.clone()]));
        {
            let app: crate::models::AppConfig = serde_json::from_value(serde_json::json!({
                "llm": [{
                    "provider": "openai",
                    "model": "gpt-4o",
                    "api_key": "sk-good-key",
                    "api_base": mock.uri(),
                    "max_tokens": 4096,
                    "temperature": 0.7
                }]
            }))
            .expect("minimal AppConfig must deserialize");
            *state.app_config.write().unwrap() = Some(Arc::new(app.clone()));
            *state.ui_config.write().unwrap() = UiConfig::from_app_config(&app);
        }

        // 1) A working key: the first read probes and reports `healthy`.
        let items = get_providers(State(state.clone())).await.0;
        assert_eq!(items["items"][0]["status"], "healthy", "got {items}");
        assert_eq!(probe_requests(&mock).await, 1, "the first read probes the provider");

        // 2) Break the key through the same path the UI uses.
        let resp = put_config(
            State(state.clone()),
            Json(serde_json::json!({ "llm": { "openaiApiKey": "sk-broken-key" } })),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            state.llm_configs.read().unwrap()[0].api_key,
            "sk-broken-key",
            "the PUT must have stored the broken key"
        );

        // 3) The next read probes the NEW credentials: the provider is 401'd
        //    and can no longer be reported healthy.
        let items = get_providers(State(state.clone())).await.0;
        assert_eq!(
            items["items"][0]["status"], "error",
            "a broken key must not keep its healthy badge: {items}"
        );
        assert_eq!(probe_requests(&mock).await, 2, "the changed key is probed, not reused");

        // 4) Restoring a working key recovers the status on the next read —
        //    via a fresh probe, because the config-change path dropped the
        //    original `sk-good-key` entry instead of leaving it for the TTL.
        let resp = put_config(
            State(state.clone()),
            Json(serde_json::json!({ "llm": { "openaiApiKey": "sk-good-key" } })),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let items = get_providers(State(state.clone())).await.0;
        assert_eq!(
            items["items"][0]["status"], "healthy",
            "restoring the key must restore the status: {items}"
        );
        assert_eq!(
            probe_requests(&mock).await,
            3,
            "each config change invalidates the entry, so every read re-probes"
        );
    }

    /// RENG-36: the provider CRUD endpoints invalidate the cached health of the
    /// config they change, so a provider edited (or removed and re-added) there
    /// is re-probed rather than answered from the entry its old config left.
    #[tokio::test]
    async fn provider_crud_invalidates_the_changed_entry() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let mut seeded = AppState::new(vec![cfg("openai")]);
        seeded.llm_health = Arc::new(crate::server::api::llm_health::LlmHealthStore::with_probe(
            Arc::new(move |_cfg| {
                counted.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {
                    Ok(crate::llm::probe::ProbeOutcome {
                        resolved_base: "stub".to_string(),
                    })
                })
            }),
            std::time::Duration::from_secs(60),
        ));
        let state = Arc::new(seeded);

        // Prime the cache for the stored config: the next read costs no probe.
        let stored = state.llm_configs.read().unwrap()[0].clone();
        state.llm_health.record(&stored, ProviderHealth::healthy(3));
        assert_eq!(
            state.llm_health.report(std::slice::from_ref(&stored)).await[0].status,
            ProviderStatus::Healthy
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // `PUT /llm/providers/openai-0` with a new key drops that entry.
        let resp = update_provider(
            State(state.clone()),
            Path("openai-0".to_string()),
            Ok(Json(UpdateProviderRequest {
                model: String::new(),
                api_key: "sk-new".to_string(),
                api_base: String::new(),
                max_tokens: 4096,
                temperature: 0.3,
            })),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let updated = state.llm_configs.read().unwrap()[0].clone();
        assert_eq!(updated.api_key, "sk-new");
        assert_eq!(
            state.llm_health.report(&[updated]).await[0].status,
            ProviderStatus::Healthy
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "the edited provider is re-probed");

        // Deleting the provider drops its entry too: re-adding the identical
        // config probes again instead of serving the deleted one's status.
        let resp = delete_provider(State(state.clone()), Path("openai-0".to_string()))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(state.llm_configs.read().unwrap().is_empty());

        let resp = add_provider(
            State(state.clone()),
            Ok(Json(AddProviderRequest {
                provider: "openai".to_string(),
                model: "openai-model".to_string(),
                api_key: "sk-new".to_string(),
                api_base: "https://api.openai.example/v1".to_string(),
                max_tokens: 4096,
                temperature: 0.3,
            })),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let readded = state.llm_configs.read().unwrap()[0].clone();
        assert_eq!(
            state.llm_health.report(&[readded]).await[0].status,
            ProviderStatus::Healthy
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a re-added provider is probed, not answered from the deleted one's entry"
        );
    }

    /// How many `GET /models` probes the fixture server received.
    async fn probe_requests(mock: &wiremock::MockServer) -> usize {
        mock.received_requests()
            .await
            .expect("wiremock records requests")
            .iter()
            .filter(|r| r.url.path() == "/models")
            .count()
    }
}
