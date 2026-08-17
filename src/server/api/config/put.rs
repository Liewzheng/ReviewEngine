use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use base64::Engine;
use std::sync::Arc;

use crate::server::AppState;

use super::types::{UiConfig, UiGitLabConfig, API_KEY_MASK};
use super::is_blank_or_masked;

/// Deep-merge `patch` into `base` (both JSON values), returning the result.
///
/// Object leaves merge key-by-key: a key present in `patch` overwrites the
/// same key in `base`, a key absent from `patch` keeps `base`'s value.
/// Non-object values (scalars, arrays, `null`) replace the base wholesale.
/// This gives `PUT /config` partial-update semantics: omitted fields keep
/// their stored value instead of being reset to a serde default.
pub fn merge_json(base: &serde_json::Value, patch: &serde_json::Value) -> serde_json::Value {
    match (base, patch) {
        (serde_json::Value::Object(base), serde_json::Value::Object(patch)) => {
            let mut merged = base.clone();
            for (key, value) in patch {
                let next = match merged.get(key) {
                    Some(existing) => merge_json(existing, value),
                    None => value.clone(),
                };
                merged.insert(key.clone(), next);
            }
            serde_json::Value::Object(merged)
        }
        (_, patch) => patch.clone(),
    }
}

/// Apply the submitted GitLab UI section to the runtime config, resolving the
/// API token with masking semantics (contract-4, aligned with LLM keys):
/// - a real value replaces the stored token;
/// - the mask sentinel `***` keeps the stored token;
/// - an empty string clears it.
///
/// Returns the resolved token (empty = unset) so the caller can persist the
/// mask/empty projection in `ui_config` and `GET /config` never leaks a live
/// token (see `mask_secrets`). Webhook secrets keep their existing
/// "non-empty overwrites, empty keeps" behavior.
pub fn apply_gitlab_runtime_config(
    gl_rt: &mut crate::server::gitlab::GitLabRuntimeConfig,
    ui_gl: &UiGitLabConfig,
) -> String {
    let submitted = ui_gl.api_token.clone();
    let new_token = if submitted.is_empty() {
        // Empty string explicitly clears the token.
        String::new()
    } else if submitted == API_KEY_MASK {
        // Mask sentinel means "keep the stored token".
        gl_rt.token.clone()
    } else {
        // A real token replaces the stored one.
        submitted
    };
    gl_rt.token = new_token.clone();

    if !ui_gl.webhook_secret.is_empty() {
        gl_rt.webhook_secret = ui_gl.webhook_secret.clone();
    }
    if !ui_gl.webhook_signing_secret.is_empty() {
        let s = ui_gl.webhook_signing_secret.clone();
        let signing_key = s
            .strip_prefix("whsec_")
            .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok());
        gl_rt.signing_secret = Some(s);
        gl_rt.signing_key = signing_key;
    }

    new_token
}

pub async fn put_config(State(state): State<Arc<AppState>>, Json(payload): Json<serde_json::Value>) -> impl IntoResponse {
    // Contract: `PUT /config` is a PARTIAL update. Only fields present in the
    // request JSON overwrite the stored config; omitted fields keep their
    // current values. A sparse PUT (e.g. just `{"rules":{"minScore":90}}`)
    // must never silently zero temperature/minScore/maxConcurrentReviews/
    // enableMetrics or drop `llm.providers`. We merge the request over a
    // snapshot of the stored UI config, then run the existing save pipeline
    // unchanged — a full-form PUT (every field present) deep-merges to exactly
    // the request, so behaviour is identical to the old wholesale replace.
    if !payload.is_object() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "config update must be a JSON object"})),
        )
            .into_response();
    }
    let mut body: UiConfig = {
        let stored = state.ui_config.read().unwrap().clone();
        // UiConfig is a plain struct of serde-native types, so serializing the
        // stored config cannot fail; the fallback is unreachable defensive code.
        let stored_json = serde_json::to_value(&stored).unwrap_or_else(|_| serde_json::json!({}));
        match serde_json::from_value(merge_json(&stored_json, &payload)) {
            Ok(ui) => ui,
            Err(e) => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({"error": format!("invalid config update: {e}")})),
                )
                    .into_response();
            }
        }
    };
    // Snapshot of currently-stored LLM configs, used to resolve "keep
    // unchanged" when the UI submits an empty or masked key (frontend "leave
    // blank = unchanged"; `GET /config` returns `***` for a configured key).
    let existing_llm = {
        let cfg_opt = state.app_config.read().unwrap();
        cfg_opt.as_ref().map(|arc| arc.llm.clone()).unwrap_or_default()
    };
    let existing_key_for = |provider: &str| -> String {
        existing_llm
            .iter()
            .find(|c| c.provider == provider)
            .map(|c| c.api_key.clone())
            .unwrap_or_default()
    };

    let mut new_llm_configs = Vec::new();

    // Legacy primary (openai): an empty or masked key means "keep the stored
    // key"; a real key replaces it.
    let mut primary_provider: Option<&str> = None;
    let openai_key = if is_blank_or_masked(&body.llm.openai_api_key) {
        existing_key_for("openai")
    } else {
        body.llm.openai_api_key.clone()
    };
    if !openai_key.is_empty() {
        primary_provider = Some("openai");
        new_llm_configs.push(crate::models::LLMConfig {
            provider: "openai".to_string(),
            model: body.llm.default_model.clone(),
            api_key: openai_key,
            api_base: body.llm.api_base_url.clone(),
            max_tokens: body.llm.max_tokens,
            temperature: body.llm.temperature,
            disable_thinking: None,
        });
    }

    // Build LLM configs from multi-provider providers Vec. GET /config maps
    // every backend LLM config — including the primary — into `llm.providers`,
    // so a UI round-trip echoes the primary back inside this array. The primary
    // is authoritatively expressed by the legacy fields above; skip providers
    // entries with the same provider name or every save would add one more
    // duplicate (the `{provider}-{i}` id scheme cannot tell them apart anyway).
    for p in &body.llm.providers {
        if p.provider.is_empty() {
            continue;
        }
        if primary_provider == Some(p.provider.as_str()) {
            continue;
        }
        // Same "keep unchanged" semantics as the legacy field: a masked key
        // must never overwrite the stored secret with the `***` sentinel.
        let key = if is_blank_or_masked(&p.api_key) {
            existing_key_for(&p.provider)
        } else {
            p.api_key.clone()
        };
        if key.is_empty() {
            continue;
        }
        new_llm_configs.push(crate::models::LLMConfig {
            provider: p.provider.clone(),
            model: p.default_model.clone(),
            api_key: key,
            api_base: p.api_base_url.clone(),
            max_tokens: p.max_tokens,
            temperature: p.temperature,
            disable_thinking: None,
        });
    }

    // Sync the persisted UI config's key fields with what was actually stored:
    // a configured provider is recorded as the mask sentinel (never a live
    // key, never a blank that would read as "unconfigured"), so GET /config
    // stays self-consistent across "leave blank = unchanged" saves.
    let has_stored_key = |provider: &str| -> bool {
        new_llm_configs
            .iter()
            .any(|c| c.provider == provider && !c.api_key.is_empty())
    };
    body.llm.openai_api_key = if has_stored_key("openai") {
        API_KEY_MASK.to_string()
    } else {
        String::new()
    };
    for p in &mut body.llm.providers {
        p.api_key = if has_stored_key(&p.provider) {
            API_KEY_MASK.to_string()
        } else {
            String::new()
        };
    }

    let mut cfg_opt = state.app_config.write().unwrap();
    if let Some(arc) = cfg_opt.as_ref() {
        let mut new_cfg = (**arc).clone();
        if !new_llm_configs.is_empty() {
            new_cfg.llm = new_llm_configs.clone();
        }
        new_cfg.max_concurrent_llm_calls = Some(body.advanced.max_concurrent_reviews as usize);
        new_cfg.max_team_size = Some(body.advanced.max_concurrent_reviews as usize);
        *cfg_opt = Some(Arc::new(new_cfg));
    } else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "config not loaded"})),
        )
            .into_response();
    }
    drop(cfg_opt);

    if !new_llm_configs.is_empty() {
        let mut llm = state.llm_configs.write().unwrap();
        *llm = new_llm_configs;
    }

    // Persist full UI config so GET /config returns exactly what was saved
    let mut ui = state.ui_config.write().unwrap();
    *ui = body;

    // Sync GitLab config to the global runtime so webhook handler picks up changes
    // without requiring a restart. The API token follows LLM-key masking
    // semantics (`***` keeps, empty clears, a real value replaces); the real
    // token lives in the runtime only, and `ui_config` persists the mask/empty
    // projection so `GET /config` never echoes it (see `mask_secrets`).
    {
        let rt = crate::server::gitlab::gitlab_runtime();
        let mut gl_rt = rt.write().unwrap();
        let resolved_token = apply_gitlab_runtime_config(&mut gl_rt, &ui.gitlab);
        ui.gitlab.api_token = if resolved_token.is_empty() {
            String::new()
        } else {
            API_KEY_MASK.to_string()
        };
    }

    Json(serde_json::json!({"status": "saved"})).into_response()
}
