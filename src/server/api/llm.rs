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

use super::llm_latency::{LatencySnapshot, LATENCY_WINDOW_DAYS};
use super::llm_probe::ProbeSnapshot;
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
    // RENG-57: how long those calls took, from the per-call samples the
    // review path recorded. Same contract: one read, `None` means every
    // latency metric is `null`.
    let latency = super::llm_latency::snapshot(&state, usage_now).await;
    // RENG-78: how long the probes themselves took, from the samples every
    // probe writes. `None` (no store / query failed) makes the two probe
    // fields `null` rather than a fabricated zero.
    let probe = super::llm_probe::snapshot(&state, usage_now).await;
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
        // RENG-57: the latency window (same length as the usage window,
        // reported separately so the client never assumes the two agree).
        "latencyWindowDays": LATENCY_WINDOW_DAYS,
        "latencySince": super::llm_latency::window_start(usage_now).to_rfc3339(),
        "latencyAvailable": latency.is_some(),
        "items": provider_items(&primary, &configs, &health, usage.as_ref(), latency.as_ref(), probe.as_ref()),
    }))
}

/// The `GET /llm/providers` card payload for one stored provider list.
///
/// `position` is the index in the STORED list (the value
/// `llm_providers.raw.position` persists, i.e. what the UI array order
/// encodes); `chainPosition` is the 1-based rank in the authoritative runtime
/// chain ([`crate::llm::chain_positions`], the primary leading) and
/// `isPrimary` marks its head — so the LLM page can show what a review will
/// actually use (RENG-55). A DISABLED provider (RENG-75) carries
/// `disabled: true` and `chainPosition: null`: it is not in the chain at all,
/// so there is no rank to show.
///
/// `health` holds one report per config, in the same order
/// ([`AppState::llm_health`](crate::server::AppState::llm_health)); `status`
/// and `lastProbeLatencyMs` come from it, never from a guess about the config
/// (RENG-36).
///
/// `usage` holds the recorded usage of the window (RENG-56), matched per CARD
/// by its entry fingerprint (RENG-75): each item folds only its own
/// fingerprint's bucket, plus the unmarked pre-upgrade bucket when it is the
/// unique enabled card of its `(provider, model)` — the fingerprint itself
/// never enters the payload. Every usage metric is `null` when the window
/// cannot be read (`None`) or holds nothing to derive it from — the page
/// renders `—`; only a count that was really measured may be `0`.
///
/// `latency` holds the recorded call latency of the window (RENG-57), read
/// and matched the same way. Its probe counterpart stays a separate field:
/// `lastProbeLatencyMs` is the instantaneous connectivity probe (RENG-36),
/// `avgLatencyMs` is the mean of the successful calls the reviews actually
/// made on that card — the RENG-53 finding was precisely that the two must
/// not be confused on screen.
///
/// `probe` holds the recorded PROBE latency (RENG-78): the mean round trip of
/// the probes themselves, which is the "communication latency" the card shows
/// first (`avgProbeLatencyMs`) — a pure network measurement, with no model
/// anywhere in it. It is matched by card fingerprint like the others, and its
/// window is the latency window (`latencyWindowDays`), so the card's three
/// numbers cover the same period.
fn provider_items(
    primary: &str,
    configs: &[crate::models::LLMConfig],
    health: &[super::llm_health::ProviderHealth],
    usage: Option<&UsageSnapshot>,
    latency: Option<&LatencySnapshot>,
    probe: Option<&ProbeSnapshot>,
) -> Vec<serde_json::Value> {
    let ranks = crate::llm::chain_positions(primary, configs);
    configs
        .iter()
        .enumerate()
        .map(|(i, cfg)| {
            let id = format!("{}-{}", cfg.provider, i);
            // `None` (a disabled entry — no chain rank) serializes as `null`.
            let chain_position = ranks.get(i).copied().flatten();
            let report = health.get(i);
            // RENG-75: this card's own fingerprint bucket, plus the unmarked
            // (pre-fingerprint) bucket when this is the unique ENABLED card
            // of its (provider, model). The fingerprint itself is
            // server-side only — the payload carries just the merged values.
            let fp = cfg.entry_fp();
            let merge_unmarked = may_merge_unmarked(configs, cfg);
            let usage = usage.map(|snapshot| snapshot.for_card(&cfg.provider, &cfg.model, &fp, merge_unmarked));
            let latency = latency.map(|snapshot| snapshot.for_card(&cfg.provider, &cfg.model, &fp, merge_unmarked));
            let probe = probe.map(|snapshot| snapshot.for_card(&cfg.provider, &fp));
            serde_json::json!({
                "id": id,
                "name": cfg.provider,
                "logo": logo_for_provider(&cfg.provider),
                "status": report
                    .map(|h| h.status.as_str())
                    .unwrap_or_else(|| super::llm_health::ProviderStatus::Offline.as_str()),
                "configured": !cfg.api_key.is_empty(),
                // RENG-75: the administrative off switch. A disabled provider
                // keeps its config and history but is out of the chain and
                // never probed (its `status` reads `disabled`, not `offline`).
                "disabled": cfg.disabled,
                // Echo the editable config back so the UI can prefill the edit
                // form. The API key is intentionally never returned.
                "apiBaseUrl": cfg.api_base,
                "defaultModel": cfg.model,
                "maxTokens": cfg.max_tokens,
                "temperature": round_temperature(cfg.temperature),
                "position": i,
                "chainPosition": chain_position,
                "isPrimary": chain_position == Some(1),
                // Round-trip time of the probe behind `status` (0 when the
                // provider was not probed — no key, or disabled). Named
                // `lastProbeLatencyMs` (RENG-57) so it can never be mistaken
                // for `avgLatencyMs` below.
                "lastProbeLatencyMs": report.map(|h| h.latency_ms).unwrap_or(0),
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
                // RENG-57: recorded call latency of the window. `avgLatencyMs`
                // is the mean of the SUCCESSFUL calls (null when the window
                // holds none); `latencySampleCount` is that mean's denominator
                // and `latencyFailureCount` the failed attempts it excludes
                // (call-level failure data RENG-56 could not see); the
                // sparkline is one point per 6-hour bucket, `null` when there
                // is no series to draw.
                "avgLatencyMs": latency.as_ref().and_then(|l| l.avg_latency_ms),
                // RENG-77 §5: avg time-to-first-byte across the successful
                // calls that actually carried one. `null` when no such call
                // exists (pre-0006 rows, registry-path calls, failed calls).
                // The field is intentionally NOT a renaming of avgLatencyMs —
                // a non-streaming provider can serve headers and body at the
                // same instant and the two numbers will be equal, which the
                // user-facing surface says so the "communication latency"
                // mental model isn't a fiction.
                "avgTtfbMs": latency.as_ref().and_then(|l| l.avg_ttfb_ms),
                // RENG-78: the mean round trip of the PROBES themselves over
                // the window — one `GET {api_base}/models` each, DNS + TCP +
                // TLS + HTTP, no model involved. This is the card's
                // "communication latency" (`平均通信延迟`): `null` when no probe
                // succeeded in the window (or the samples could not be read),
                // never a fabricated `0`. `probeSampleCount` is that mean's
                // denominator (successful probes only; failures are recorded in
                // the table but excluded here, like the call samples).
                "avgProbeLatencyMs": probe.and_then(|p| p.avg_latency_ms),
                "probeSampleCount": probe.map(|p| p.sample_count),
                "latencySampleCount": latency.as_ref().map(|l| l.sample_count),
                "latencyFailureCount": latency.as_ref().map(|l| l.failure_count),
                "latencyLastSampleAt": latency
                    .as_ref()
                    .and_then(|l| l.last_sample_at)
                    .map(|t| t.to_rfc3339()),
                "latencySparkline": latency.as_ref().and_then(|l| l.sparkline.clone()),
                // Timestamp of the probe behind `status`; `null` when no probe
                // happened — for an `offline` provider (no key, never probed),
                // a `disabled` one (RENG-75: deliberately off, never probed),
                // or a config with no report at all. Never "now", which would
                // claim a check that did not happen.
                "lastChecked": report
                    .filter(|h| {
                        !matches!(
                            h.status,
                            super::llm_health::ProviderStatus::Offline | super::llm_health::ProviderStatus::Disabled
                        )
                    })
                    .map(|h| h.checked_at.to_rfc3339()),
            })
        })
        .collect()
}

/// RENG-75 upgrade rule: the pre-fingerprint ("unmarked") statistics bucket
/// of a `(provider, model)` pair is merged ONLY into the unique ENABLED card
/// with that pair — the one attribution that cannot be wrong. With several
/// enabled candidates, or none, the bucket is shown nowhere (the rows stay in
/// the database; they still count toward the window totals).
fn may_merge_unmarked(configs: &[crate::models::LLMConfig], cfg: &crate::models::LLMConfig) -> bool {
    !cfg.disabled
        && configs
            .iter()
            .filter(|c| !c.disabled && c.provider == cfg.provider && c.model == cfg.model)
            .count()
            == 1
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
        disabled: false,
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
    use crate::store::traits::{LlmCallSampleRow, ProviderUsageStats};

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
            disabled: false,
        }
    }

    /// Health reports matching `configs` (the shape `AppState::llm_health`
    /// produces), used by the payload tests that are not about probing.
    fn healthy(n: usize) -> Vec<ProviderHealth> {
        (0..n).map(|_| ProviderHealth::healthy(7)).collect()
    }

    /// A usage bucket attributed to `c`'s own fingerprint (RENG-75).
    fn usage_stats(
        c: &crate::models::LLMConfig,
        usage: u64,
        completed: u64,
        failed: u64,
        last: Option<chrono::DateTime<chrono::Utc>>,
    ) -> ProviderUsageStats {
        ProviderUsageStats {
            provider: c.provider.clone(),
            model: c.model.clone(),
            fp: Some(c.entry_fp()),
            usage_count: usage,
            completed_count: completed,
            failed_count: failed,
            last_used_at: last,
        }
    }

    /// RENG-55: the card payload exposes the chain, not just the stored list —
    /// `position` stays the stored index, `chainPosition`/`isPrimary` describe
    /// the runtime order the page renders.
    #[test]
    fn provider_items_expose_chain_position_and_primary() {
        let stored = vec![cfg("xiaomi"), cfg("deepseek")];
        let items = provider_items("deepseek", &stored, &healthy(stored.len()), None, None, None);

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
        let items = provider_items("", &stored, &healthy(stored.len()), None, None, None);
        assert_eq!(items[0]["isPrimary"], true);
        assert_eq!(items[1]["isPrimary"], false);
        assert_eq!(items[1]["chainPosition"], 2);
        // A primary naming a provider the runtime no longer holds (stale
        // `primaryProvider` in the echo) degrades to the same rule.
        let items = provider_items("ghost", &[cfg("xiaomi")], &healthy(1), None, None, None);
        assert_eq!(items[0]["isPrimary"], true);
        assert_eq!(items[0]["chainPosition"], 1);
    }

    /// Empty provider set → empty payload, no panic.
    #[test]
    fn provider_items_empty_set() {
        assert!(provider_items("deepseek", &[], &[], None, None, None).is_empty());
    }

    /// RENG-75: a disabled provider's card tells "deliberately off", never a
    /// failure and never a chain member: `disabled: true`, `status:
    /// "disabled"` (NOT `offline`), `chainPosition: null`, `isPrimary: false`,
    /// `lastChecked: null` (no probe happens). The enabled neighbours number
    /// the chain without it (positions 1 and 2), and its recorded usage/latency
    /// history stays visible — disabling hides nothing but the chain rank.
    #[test]
    fn provider_items_mark_a_disabled_provider_off_the_chain() {
        let mut stored = vec![cfg("xiaomi"), cfg("deepseek"), cfg("openai")];
        stored[1].disabled = true;
        let items = provider_items(
            "xiaomi",
            &stored,
            &[
                ProviderHealth::healthy(7),
                ProviderHealth::disabled(),
                ProviderHealth::healthy(9),
            ],
            None,
            None,
            None,
        );

        // Stored order and ids untouched; the disabled card keeps its slot.
        assert_eq!(items[1]["name"], "deepseek");
        assert_eq!(items[1]["id"], "deepseek-1");
        assert_eq!(items[1]["position"], 1);
        assert_eq!(items[1]["disabled"], true);
        assert_eq!(items[0]["disabled"], false);
        assert_eq!(items[2]["disabled"], false);

        // Deliberately off ≠ unreachable, and it is no chain member.
        assert_eq!(items[1]["status"], "disabled");
        assert_eq!(items[1]["chainPosition"], serde_json::Value::Null);
        assert_eq!(items[1]["isPrimary"], false);
        assert_eq!(items[1]["lastProbeLatencyMs"], 0);
        assert_eq!(items[1]["lastChecked"], serde_json::Value::Null);

        // The enabled subset numbers the chain 1, 2 — the head is the first
        // enabled entry.
        assert_eq!(items[0]["chainPosition"], 1);
        assert_eq!(items[0]["isPrimary"], true);
        assert_eq!(items[2]["chainPosition"], 2);
        assert_eq!(items[2]["isPrimary"], false);

        // A recorded primary naming the disabled entry cannot put it back at
        // the head: the first enabled provider leads.
        let items = provider_items(
            "deepseek",
            &stored,
            &[
                ProviderHealth::healthy(7),
                ProviderHealth::disabled(),
                ProviderHealth::healthy(9),
            ],
            None,
            None,
            None,
        );
        assert_eq!(items[1]["chainPosition"], serde_json::Value::Null);
        assert_eq!(items[0]["chainPosition"], 1);
        assert_eq!(items[0]["isPrimary"], true);
    }

    /// RENG-36: the card's `status`/`lastProbeLatencyMs`/`lastChecked` come
    /// from the probe report — never from a guess about the config's shape. A
    /// provider with a key can therefore be `error`, which is the whole point:
    /// it used to be reported `healthy` for no other reason than having a key.
    ///
    /// RENG-57 renamed the probe's field to `lastProbeLatencyMs` so the
    /// instantaneous probe and the recorded `avgLatencyMs` cannot be confused.
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
            None,
            None,
        );

        assert_eq!(items[0]["status"], "error");
        assert_eq!(items[0]["configured"], true, "a key is still stored");
        assert_eq!(items[0]["lastProbeLatencyMs"], 12);
        assert_eq!(items[1]["status"], "healthy");
        assert_eq!(items[1]["lastProbeLatencyMs"], 34);
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
        let items = provider_items("xiaomi", &[blank], &[ProviderHealth::offline()], None, None, None);
        assert_eq!(items[0]["status"], "offline");
        assert_eq!(items[0]["configured"], false);
        assert_eq!(items[0]["lastProbeLatencyMs"], 0);
        // RENG-56: a provider that was never probed has no check time — the
        // payload says `null` instead of stamping the current time.
        assert_eq!(items[0]["lastChecked"], serde_json::Value::Null);
        // A config with no report at all (misaligned health list) is the same.
        let items = provider_items("xiaomi", &stored, &[], None, None, None);
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
            vec![usage_stats(&stored[0], 4, 4, 0, Some(chrono::Utc::now()))],
        );
        let items = provider_items("xiaomi", &stored, &healthy(1), Some(&snapshot), None, None);
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
        let items = provider_items("xiaomi", &stored, &healthy(1), None, None, None);
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
        let items = provider_items("xiaomi", &stored, &healthy(1), Some(&snapshot), None, None);

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
                usage_stats(&stored[0], 3, 3, 0, Some(last_used)),
                usage_stats(&stored[1], 1, 0, 1, None),
            ],
        );
        let items = provider_items("xiaomi", &stored, &healthy(2), Some(&snapshot), None, None);

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
            disabled: false,
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

    // ─── RENG-75: per-fingerprint card statistics ─────────────────

    /// One of two same-named accounts: identical `(provider, api_base,
    /// model)` triple, distinct key → distinct fingerprint.
    fn account(key: &str) -> crate::models::LLMConfig {
        crate::models::LLMConfig {
            provider: "acme".to_string(),
            model: "m1".to_string(),
            api_key: key.to_string(),
            api_base: "https://api.acme.example/v1".to_string(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
            disabled: false,
        }
    }

    /// An unmarked (pre-fingerprint) usage bucket for `(acme, m1)`.
    fn unmarked_usage(usage: u64, completed: u64, failed: u64) -> ProviderUsageStats {
        ProviderUsageStats {
            provider: "acme".to_string(),
            model: "m1".to_string(),
            fp: None,
            usage_count: usage,
            completed_count: completed,
            failed_count: failed,
            last_used_at: None,
        }
    }

    fn latency_row_for(
        card: &crate::models::LLMConfig,
        at: chrono::DateTime<chrono::Utc>,
        ms: i64,
    ) -> LlmCallSampleRow {
        LlmCallSampleRow {
            provider: card.provider.clone(),
            model: card.model.clone(),
            entry_fp: Some(card.entry_fp()),
            created_at: at,
            latency_ms: ms,
            ttfb_ms: None,
            success: true,
        }
    }

    fn unmarked_latency_row(at: chrono::DateTime<chrono::Utc>, ms: i64) -> LlmCallSampleRow {
        LlmCallSampleRow {
            provider: "acme".to_string(),
            model: "m1".to_string(),
            entry_fp: None,
            created_at: at,
            latency_ms: ms,
            ttfb_ms: None,
            success: true,
        }
    }

    /// The merge decision itself (RENG-75 升级规则): the unmarked bucket of a
    /// `(provider, model)` pair goes ONLY to the unique ENABLED card of that
    /// pair — never to a disabled card, never to either of two enabled
    /// candidates.
    #[test]
    fn unmarked_merge_requires_a_unique_enabled_card() {
        let a = account("sk-a");
        let b = account("sk-b");

        // A single card, and it is enabled → merge.
        assert!(may_merge_unmarked(std::slice::from_ref(&a), &a), "single enabled card");
        // The same card disabled → no merge (无启用卡不并入).
        let mut off = a.clone();
        off.disabled = true;
        assert!(
            !may_merge_unmarked(std::slice::from_ref(&off), &off),
            "the only card is disabled"
        );
        // Two enabled same-pair cards → no merge for either (多卡不并入).
        assert!(
            !may_merge_unmarked(&[a.clone(), b.clone()], &a),
            "two enabled candidates"
        );
        assert!(
            !may_merge_unmarked(&[a.clone(), b.clone()], &b),
            "two enabled candidates"
        );
        // One enabled + one disabled same-pair card → the enabled one is
        // still the unique ENABLED card and merges; the disabled one doesn't.
        let mut off_b = b.clone();
        off_b.disabled = true;
        let pair = [a.clone(), off_b.clone()];
        assert!(may_merge_unmarked(&pair, &a), "unique enabled among a mixed pair");
        assert!(!may_merge_unmarked(&pair, &off_b), "the disabled card never merges");
    }

    /// Two same-named cards on the page report only their own fingerprint's
    /// numbers — usage AND latency — and the unmarked (pre-upgrade) buckets
    /// merge into NEITHER when two enabled cards share the pair. The
    /// fingerprint itself never appears in the payload.
    #[test]
    fn provider_items_same_named_cards_report_only_their_own_stats() {
        let a = account("sk-a");
        let b = account("sk-b");
        let stored = vec![a.clone(), b.clone()];
        assert_ne!(a.entry_fp(), b.entry_fp(), "two accounts, two fingerprints");

        let usage = UsageSnapshot::new(
            chrono::Utc::now(),
            vec![
                usage_stats(&a, 3, 3, 0, None),
                usage_stats(&b, 1, 0, 1, None),
                unmarked_usage(5, 5, 0),
            ],
        );
        let now = chrono::Utc::now();
        let latency = latency_snapshot(vec![
            latency_row_for(&a, now - chrono::Duration::minutes(3), 100),
            latency_row_for(&a, now - chrono::Duration::minutes(2), 300),
            latency_row_for(&b, now - chrono::Duration::minutes(1), 900),
            unmarked_latency_row(now - chrono::Duration::minutes(4), 50),
        ]);
        let items = provider_items("acme", &stored, &healthy(2), Some(&usage), Some(&latency), None);

        // Card A: its own 3 usages / 2 samples at mean 200.
        assert_eq!(items[0]["requestCount"], 3);
        assert_eq!(items[0]["usageShare"], 0.3333, "3 of 9 total");
        assert_eq!(items[0]["successRate"], 1.0);
        assert_eq!(items[0]["avgLatencyMs"], 200);
        assert_eq!(items[0]["latencySampleCount"], 2);
        // Card B: its own 1 usage / 1 sample at 900 — never A's numbers.
        assert_eq!(items[1]["requestCount"], 1);
        assert_eq!(items[1]["usageShare"], 0.1111, "1 of 9 total");
        assert_eq!(items[1]["successRate"], 0.0);
        assert_eq!(items[1]["avgLatencyMs"], 900);
        assert_eq!(items[1]["latencySampleCount"], 1);

        // The unmarked rows went to neither card (two enabled candidates),
        // but still count in the window totals the shares divide by.
        let fp_of = |card: &crate::models::LLMConfig| card.entry_fp();
        for item in &items {
            let text = serde_json::to_string(item).unwrap();
            assert!(!text.contains(&fp_of(&a)), "no fingerprint in the payload: {text}");
            assert!(!text.contains(&fp_of(&b)), "no fingerprint in the payload: {text}");
            assert!(!item.as_object().unwrap().contains_key("fp"));
            assert!(!item.as_object().unwrap().contains_key("entryFp"));
        }
    }

    /// The single-enabled-card case, end to end: the pre-upgrade (unmarked)
    /// usage AND latency fold into that card's numbers.
    #[test]
    fn provider_items_merge_unmarked_stats_into_the_unique_enabled_card() {
        let a = account("sk-a");
        let stored = vec![a.clone()];
        let usage = UsageSnapshot::new(
            chrono::Utc::now(),
            vec![usage_stats(&a, 2, 2, 0, None), unmarked_usage(5, 4, 1)],
        );
        let now = chrono::Utc::now();
        let latency = latency_snapshot(vec![
            latency_row_for(&a, now - chrono::Duration::minutes(3), 100),
            unmarked_latency_row(now - chrono::Duration::minutes(2), 300),
            unmarked_latency_row(now - chrono::Duration::minutes(1), 500),
        ]);
        let items = provider_items("acme", &stored, &healthy(1), Some(&usage), Some(&latency), None);

        assert_eq!(items[0]["requestCount"], 7, "2 own + 5 unmarked");
        assert_eq!(items[0]["successRate"], 0.8571, "6 completed of 7 decided");
        assert_eq!(items[0]["avgLatencyMs"], 300, "(100+300+500)/3 — merged as raw sums");
        assert_eq!(items[0]["latencySampleCount"], 3);
    }

    // ─── RENG-57: recorded call latency ─────────────────────────────

    fn latency_row(provider: &str, at: chrono::DateTime<chrono::Utc>, ms: i64, success: bool) -> LlmCallSampleRow {
        let c = cfg(provider);
        LlmCallSampleRow {
            provider: c.provider.clone(),
            model: c.model.clone(),
            entry_fp: Some(c.entry_fp()),
            created_at: at,
            latency_ms: ms,
            ttfb_ms: None,
            success,
        }
    }

    fn latency_snapshot(rows: Vec<LlmCallSampleRow>) -> LatencySnapshot {
        LatencySnapshot::new(
            chrono::Utc::now() - chrono::Duration::days(super::super::llm_latency::LATENCY_WINDOW_DAYS),
            rows,
        )
    }

    /// The card carries BOTH measurements, under names that cannot be
    /// confused: `lastProbeLatencyMs` (RENG-36's instantaneous probe) and
    /// `avgLatencyMs` (RENG-57's recorded window average) — the RENG-53
    /// finding was exactly this conflation.
    #[test]
    fn provider_items_report_recorded_latency_next_to_the_probe() {
        let stored = vec![cfg("xiaomi")];
        let now = chrono::Utc::now();
        let snapshot = latency_snapshot(vec![
            latency_row("xiaomi", now - chrono::Duration::minutes(3), 100, true),
            latency_row("xiaomi", now - chrono::Duration::minutes(2), 200, true),
            // A failure: counted, kept out of the average.
            latency_row("xiaomi", now - chrono::Duration::minutes(1), 90_000, false),
        ]);
        let items = provider_items("xiaomi", &stored, &healthy(1), None, Some(&snapshot), None);
        let item = items[0].as_object().unwrap();

        assert_eq!(item["lastProbeLatencyMs"], 7, "the probe's own round trip");
        assert_eq!(item["avgLatencyMs"], 150, "mean of the two successful calls");
        assert_eq!(item["latencySampleCount"], 2);
        assert_eq!(item["latencyFailureCount"], 1);
        assert!(item["latencyLastSampleAt"].is_string());
        assert!(
            !item.contains_key("latencyMs"),
            "the ambiguous name is gone for good: {item:?}"
        );
        let series = item["latencySparkline"].as_array().unwrap();
        assert_eq!(series.len(), super::super::llm_latency::SPARKLINE_BUCKETS);
        assert_eq!(
            series.iter().filter(|v| !v.is_null()).count(),
            1,
            "all three calls fall in the newest bucket"
        );
    }

    /// Without a readable sample table (no store, failed query) every latency
    /// metric is `null` — the page's `—`, never a fabricated `0 ms`.
    #[test]
    fn provider_items_without_samples_report_null_latency() {
        let stored = vec![cfg("xiaomi")];
        // No snapshot at all (no store attached / query failed).
        let items = provider_items("xiaomi", &stored, &healthy(1), None, None, None);
        for field in [
            "avgLatencyMs",
            "latencySampleCount",
            "latencyFailureCount",
            "latencyLastSampleAt",
            "latencySparkline",
        ] {
            assert_eq!(items[0][field], serde_json::Value::Null, "{field} must be null");
        }

        // A snapshot that holds nothing for this provider: measured zero
        // counts, `null` for everything derived (including the series — there
        // is nothing to draw).
        let items = provider_items(
            "xiaomi",
            &stored,
            &healthy(1),
            None,
            Some(&latency_snapshot(Vec::new())),
            None,
        );
        assert_eq!(items[0]["latencySampleCount"], 0);
        assert_eq!(items[0]["latencyFailureCount"], 0);
        assert_eq!(
            items[0]["avgLatencyMs"],
            serde_json::Value::Null,
            "0/0 is not a latency"
        );
        assert_eq!(items[0]["latencySparkline"], serde_json::Value::Null);
    }

    /// The whole path the page reads: samples recorded through the sink become
    /// the card's average, and that average matches an independent
    /// computation over the rows the store holds.
    #[tokio::test]
    async fn get_providers_reports_the_recorded_latency_average_from_the_store() {
        use crate::store::llm_samples::StoreLlmCallSink;

        let store = Arc::new(crate::store::SqlxStore::new_in_memory().await.unwrap());
        store.migrate().await.unwrap();
        let now = chrono::Utc::now();

        // Two reviews' worth of calls, written through the real sink (the
        // only writer in production): 4 successful xiaomi calls and one
        // failed one, plus a deepseek call outside the window.
        let sink = StoreLlmCallSink::shared(store.clone(), Some("review-1".to_string()));
        for (provider, ago_minutes, latency_ms, success) in [
            ("xiaomi", 5, 100u64, true),
            ("xiaomi", 4, 200, true),
            ("xiaomi", 3, 300, true),
            ("xiaomi", 2, 400, true),
            ("xiaomi", 1, 30_000, false),
            ("deepseek", 1, 50, true),
        ] {
            sink.record(&crate::llm::sampling::LlmCallSample {
                at: now - chrono::Duration::minutes(ago_minutes),
                provider: provider.to_string(),
                model: format!("{provider}-model"),
                entry_fp: cfg(provider).entry_fp(),
                latency_ms,
                ttfb_ms: None,
                success,
                error: (!success).then(|| "HTTP 500".to_string()),
                chain_position: 1,
                attempt: 1,
            })
            .await;
        }
        // Outside the window: never aggregated.
        sink.record(&crate::llm::sampling::LlmCallSample {
            at: now - chrono::Duration::days(30),
            provider: "xiaomi".to_string(),
            model: "mimo".to_string(),
            entry_fp: "fp-mimo".to_string(),
            latency_ms: 9999,
            ttfb_ms: None,
            success: true,
            error: None,
            chain_position: 1,
            attempt: 1,
        })
        .await;

        let mut state = stub_state(vec![cfg("xiaomi"), cfg("deepseek")]);
        Arc::get_mut(&mut state).unwrap().db = Some(store.clone());
        let payload = get_providers(State(state)).await.0;

        assert_eq!(payload["latencyWindowDays"], 7);
        assert_eq!(payload["latencyAvailable"], true);
        assert!(
            chrono::DateTime::parse_from_rfc3339(payload["latencySince"].as_str().unwrap()).is_ok(),
            "the window start travels as RFC3339: {payload}"
        );

        let xiaomi = &payload["items"][0];
        assert_eq!(xiaomi["avgLatencyMs"], 250, "mean of 100/200/300/400");
        assert_eq!(xiaomi["latencySampleCount"], 4);
        assert_eq!(xiaomi["latencyFailureCount"], 1);
        // Independent computation over the raw rows, for the record.
        let rows = crate::store::traits::ReviewStore::llm_samples_since(
            store.as_ref(),
            super::super::llm_latency::window_start(chrono::Utc::now()),
        )
        .await
        .unwrap();
        let (sum, count) = rows
            .iter()
            .filter(|r| r.provider == "xiaomi" && r.success)
            .fold((0i64, 0i64), |(s, c), r| (s + r.latency_ms, c + 1));
        assert_eq!(
            sum / count,
            xiaomi["avgLatencyMs"].as_i64().unwrap(),
            "the payload's average must equal the mean of the stored rows"
        );

        let deepseek = &payload["items"][1];
        assert_eq!(deepseek["avgLatencyMs"], 50);
        assert_eq!(deepseek["latencySampleCount"], 1);
        assert_eq!(deepseek["latencyFailureCount"], 0);
    }

    /// With no store the latency metrics are `null` and nothing else about the
    /// card changes: the page must keep working with `REVIEW_DISABLE_DB=1`.
    #[tokio::test]
    async fn get_providers_without_a_store_reports_unknown_latency() {
        let state = stub_state(vec![cfg("xiaomi")]);
        let payload = get_providers(State(state)).await.0;

        assert_eq!(payload["latencyAvailable"], false);
        let item = &payload["items"][0];
        assert_eq!(item["name"], "xiaomi");
        assert_eq!(item["status"], "healthy");
        assert_eq!(item["avgLatencyMs"], serde_json::Value::Null);
        assert_eq!(item["latencySampleCount"], serde_json::Value::Null);
        assert_eq!(item["latencySparkline"], serde_json::Value::Null);
        // The probe's own field is still there and still the probe's value
        // (the stubbed probe in this test measures 0 ms): the two numbers
        // never share a field.
        assert_eq!(item["lastProbeLatencyMs"], 0);
        assert!(!item.as_object().unwrap().contains_key("latencyMs"));
    }

    /// RENG-77 §5: `avgTtfbMs` is emitted alongside `avgLatencyMs` and is
    /// `null` when the window holds no successful sample that carries a
    /// TTFB (the legacy / registry-path shape). Both metrics are computed
    /// independently and may legitimately be equal for a non-streaming
    /// provider (see `ttfb_equals_latency_*` in `llm_latency.rs`).
    #[test]
    fn provider_items_emit_avg_ttfb_null_when_no_sample_carries_one() {
        let stored = vec![cfg("xiaomi")];
        let now = chrono::Utc::now();
        // The only successful call has `ttfb_ms: None` (registry-path shape).
        let snapshot = latency_snapshot(vec![latency_row("xiaomi", now, 100, true)]);
        let items = provider_items("xiaomi", &stored, &healthy(1), None, Some(&snapshot), None);
        let item = &items[0];
        assert_eq!(item["avgLatencyMs"], 100, "the latency average still computes");
        assert_eq!(
            item["avgTtfbMs"],
            serde_json::Value::Null,
            "no TTFB measurements in the window → null on the wire"
        );
    }

    /// RENG-77 §5: when the window has measured TTFB values, the payload
    /// carries the rounded mean. The verdict (`avgTtfbMs ≈ avgLatencyMs` for
    /// non-streaming providers) is asserted inside `llm_latency.rs`'s
    /// `ttfb_equals_latency_*`; this test only proves the field reaches the
    /// wire on the card the page reads.
    #[test]
    fn provider_items_emit_avg_ttfb_when_samples_carry_it() {
        let stored = vec![cfg("xiaomi")];
        let now = chrono::Utc::now();
        // Two successful calls WITH ttfb values.
        let since = chrono::Utc::now() - chrono::Duration::days(super::super::llm_latency::LATENCY_WINDOW_DAYS);
        let snapshot = LatencySnapshot::new(
            since,
            vec![
                LlmCallSampleRow {
                    provider: "xiaomi".to_string(),
                    model: "xiaomi-model".to_string(),
                    entry_fp: Some(cfg("xiaomi").entry_fp()),
                    created_at: now,
                    latency_ms: 200,
                    ttfb_ms: Some(180),
                    success: true,
                },
                LlmCallSampleRow {
                    provider: "xiaomi".to_string(),
                    model: "xiaomi-model".to_string(),
                    entry_fp: Some(cfg("xiaomi").entry_fp()),
                    created_at: now,
                    latency_ms: 200,
                    ttfb_ms: Some(220),
                    success: true,
                },
            ],
        );
        let items = provider_items("xiaomi", &stored, &healthy(1), None, Some(&snapshot), None);
        let item = &items[0];
        assert_eq!(item["avgLatencyMs"], 200);
        assert_eq!(
            item["avgTtfbMs"], 200,
            "(180 + 220) / 2 rounds to 200 — the field is on the wire"
        );
    }

    // ─── RENG-78: the probe's own communication latency ─────────────────

    fn probe_snapshot(rows: Vec<crate::store::traits::ProbeSample>) -> ProbeSnapshot {
        ProbeSnapshot::new(rows)
    }

    fn probe_row(
        provider: &str,
        at: chrono::DateTime<chrono::Utc>,
        latency_ms: Option<i64>,
        success: bool,
    ) -> crate::store::traits::ProbeSample {
        crate::store::traits::ProbeSample {
            provider: provider.to_string(),
            entry_fp: cfg(provider).entry_fp(),
            at,
            latency_ms,
            success,
            error: (!success).then(|| "HTTP 401 Unauthorized".to_string()),
        }
    }

    /// The card carries the probe average next to the call metrics, under a
    /// name of its own — and the probe's own field is still the probe's number.
    #[test]
    fn provider_items_report_the_probe_average() {
        let stored = vec![cfg("xiaomi")];
        let now = chrono::Utc::now();
        let snapshot = probe_snapshot(vec![
            probe_row("xiaomi", now - chrono::Duration::minutes(30), Some(30), true),
            probe_row("xiaomi", now - chrono::Duration::minutes(5), Some(50), true),
            // A failed probe: recorded, excluded from the average.
            probe_row("xiaomi", now, None, false),
        ]);
        let items = provider_items("xiaomi", &stored, &healthy(1), None, None, Some(&snapshot));
        let item = items[0].as_object().unwrap();
        assert_eq!(item["avgProbeLatencyMs"], 40, "mean of the two successful probes");
        assert_eq!(item["probeSampleCount"], 2, "the successful probes behind it");
        assert_eq!(
            item["lastProbeLatencyMs"], 7,
            "the instantaneous probe field keeps its own meaning (RENG-36)"
        );
        assert_eq!(
            item["avgLatencyMs"],
            serde_json::Value::Null,
            "no call samples in this snapshot: the call average is untouched and still null"
        );
    }

    /// No probe succeeded in the window → `null`, never `0 ms`; and without a
    /// readable probe table the count is `null` too (unknown, not zero).
    #[test]
    fn provider_items_report_a_null_probe_average_when_nothing_succeeded() {
        let stored = vec![cfg("xiaomi")];
        let now = chrono::Utc::now();
        let items = provider_items(
            "xiaomi",
            &stored,
            &healthy(1),
            None,
            None,
            Some(&probe_snapshot(vec![probe_row("xiaomi", now, None, false)])),
        );
        assert_eq!(items[0]["avgProbeLatencyMs"], serde_json::Value::Null);
        assert_eq!(
            items[0]["probeSampleCount"], 0,
            "measured: one probe, none of them good"
        );

        // No snapshot at all (no store attached / query failed).
        let items = provider_items("xiaomi", &stored, &healthy(1), None, None, None);
        assert_eq!(items[0]["avgProbeLatencyMs"], serde_json::Value::Null);
        assert_eq!(items[0]["probeSampleCount"], serde_json::Value::Null);
    }

    /// The whole path the page reads: the probes the health store runs become
    /// the card's `avgProbeLatencyMs`, and the number equals the mean of the
    /// rows the store holds — computed here independently.
    #[tokio::test]
    async fn get_providers_reports_the_probe_average_from_the_store() {
        use crate::store::traits::{ProbeSample, ReviewStore};

        let store = Arc::new(crate::store::SqlxStore::new_in_memory().await.unwrap());
        store.migrate().await.unwrap();
        let now = chrono::Utc::now();
        // Two successful probes of THIS card, one of a same-named sibling, and
        // one failure.
        let xiaomi = cfg("xiaomi");
        let sibling = crate::models::LLMConfig {
            api_key: "other-key".to_string(),
            ..xiaomi.clone()
        };
        assert_ne!(
            xiaomi.entry_fp(),
            sibling.entry_fp(),
            "a different key is a different card"
        );
        for (fp, latency, success) in [
            (xiaomi.entry_fp(), Some(40i64), true),
            (xiaomi.entry_fp(), Some(60), true),
            (sibling.entry_fp(), Some(9000), true),
            (xiaomi.entry_fp(), None, false),
        ] {
            store
                .insert_probe_sample(&ProbeSample {
                    provider: "xiaomi".to_string(),
                    entry_fp: fp,
                    at: now,
                    latency_ms: latency,
                    success,
                    error: (!success).then(|| "HTTP 401 Unauthorized".to_string()),
                })
                .await
                .unwrap();
        }

        let mut state = stub_state(vec![xiaomi.clone()]);
        Arc::get_mut(&mut state).unwrap().db = Some(store.clone());
        let payload = get_providers(State(state)).await.0;

        let item = &payload["items"][0];
        assert_eq!(
            item["avgProbeLatencyMs"], 50,
            "mean of this card's two successful probes"
        );
        assert_eq!(item["probeSampleCount"], 2);
        // Independent computation over the raw rows, for the record.
        let rows = store
            .probe_samples_since(crate::server::api::llm_probe::window_start(now))
            .await
            .unwrap();
        let own: Vec<&ProbeSample> = rows
            .iter()
            .filter(|r| r.entry_fp == xiaomi.entry_fp() && r.success)
            .collect();
        let sum: i64 = own.iter().filter_map(|r| r.latency_ms).sum();
        assert_eq!(
            sum / own.len() as i64,
            item["avgProbeLatencyMs"].as_i64().unwrap(),
            "the payload's average must equal the mean of the stored probes"
        );
    }

    /// With no store the two probe fields are `null` and nothing else about the
    /// card changes (`REVIEW_DISABLE_DB=1`).
    #[tokio::test]
    async fn get_providers_without_a_store_reports_unknown_probe_latency() {
        let state = stub_state(vec![cfg("xiaomi")]);
        let payload = get_providers(State(state)).await.0;
        let item = &payload["items"][0];
        assert_eq!(item["avgProbeLatencyMs"], serde_json::Value::Null);
        assert_eq!(item["probeSampleCount"], serde_json::Value::Null, "unknown, not zero");
        assert_eq!(item["name"], "xiaomi");
        assert_eq!(item["status"], "healthy");
    }

    /// The full round trip the server runs: a health store with the sample sink
    /// attached probes a provider, and the next `GET /llm/providers` reports the
    /// probe's own latency as the card's communication latency.
    #[tokio::test]
    async fn a_probe_through_the_health_store_reaches_the_card() {
        use crate::server::api::llm_probe::{store_sink, window_start};
        use crate::store::traits::ReviewStore;

        let store = Arc::new(crate::store::SqlxStore::new_in_memory().await.unwrap());
        store.migrate().await.unwrap();
        let mut state = stub_state(vec![cfg("xiaomi")]);
        // The same sink `serve` attaches at startup, over a real table.
        state
            .llm_health
            .attach_sample_sink(Some(store_sink(Arc::clone(&store) as Arc<dyn ReviewStore>)));
        Arc::get_mut(&mut state).unwrap().db = Some(store.clone());

        let configs = state.llm_configs.read().unwrap().clone();
        state.llm_health.probe_all(&configs).await;

        let rows = store
            .probe_samples_since(window_start(chrono::Utc::now()))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "the round wrote exactly one sample");
        assert_eq!(rows[0].entry_fp, configs[0].entry_fp());
        assert!(rows[0].success);

        let payload = get_providers(State(state)).await.0;
        let item = &payload["items"][0];
        assert_eq!(item["probeSampleCount"], 1, "the sample the round just wrote");
        assert!(
            item["avgProbeLatencyMs"].is_i64(),
            "and it is what the card averages: {item}"
        );
    }
}
