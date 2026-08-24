use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use base64::Engine;
use std::sync::Arc;

use crate::server::AppState;

use super::is_blank_or_masked;
use super::types::{UiConfig, UiGitLabConfig, UiGitPlatformConfig, API_KEY_MASK};

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

/// The masked/empty projection of a secret for the UI layer: `***` when set,
/// empty string when unset — the same masking semantics as LLM keys, so
/// `GET /config` never leaks a live secret.
fn mask_or_empty(secret: &str) -> String {
    if secret.is_empty() {
        String::new()
    } else {
        API_KEY_MASK.to_string()
    }
}

/// Resolve the submitted `gitPlatforms` array into the new live set.
///
/// Semantics: when the `gitPlatforms` key is present, the submitted array
/// REPLACES the full configured set (to delete an entry, submit the array
/// without it) — except that a blank or masked (`***`) token / webhookSecret
/// / webhookSigningSecret on an entry keeps the stored secret of the SAME
/// (name, baseUrl) entry, identical to how additional LLM providers save.
/// Matching on the pair, not the name alone, is deliberate: a same-named
/// entry pointing at a DIFFERENT instance must not silently inherit the old
/// instance's credentials (cross-instance secret leakage), and renaming an
/// entry (name change, same baseUrl) likewise drops the secrets — the user
/// re-enters them after a rename or a baseUrl change. When the key is absent
/// from the PUT payload, the merge with the stored config carries the
/// existing (masked) list over, so the set round-trips unchanged. Duplicate
/// names in one submission: last write wins.
fn resolve_git_platforms(
    submitted: &[UiGitPlatformConfig],
    existing: &[crate::models::GitPlatformConfig],
) -> Result<Vec<crate::models::GitPlatformConfig>, (StatusCode, Json<serde_json::Value>)> {
    let mut resolved: Vec<crate::models::GitPlatformConfig> = Vec::new();
    for p in submitted {
        let name = p.name.trim();
        if name.is_empty() {
            continue;
        }
        let platform_type = {
            let t = p.platform_type.trim();
            if t.is_empty() {
                "gitlab".to_string()
            } else {
                t.to_ascii_lowercase()
            }
        };
        if platform_type != "gitlab" {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": format!(
                        "unsupported git platform type '{}' for entry '{}' (only 'gitlab' is implemented)",
                        p.platform_type, name
                    )
                })),
            ));
        }
        let base_url = p.base_url.trim().trim_end_matches('/').to_string();
        // baseUrl keys both the credential routing (host:port matching at
        // review/webhook time) and the secret-keep below, so an empty,
        // unparseable, or non-http(s) value is a hard 422 — never a silently
        // stored broken entry. The authority check closes a `url`-crate
        // quirk: `http:///host` parses successfully with host "host" (the
        // empty authority is collapsed), so the original string must carry a
        // non-empty authority between `scheme://` and the next `/`.
        let valid_base = reqwest::Url::parse(&base_url)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
            .filter(|_| {
                base_url
                    .split_once("://")
                    .is_some_and(|(_, rest)| !rest.is_empty() && !rest.starts_with('/'))
            });
        if valid_base.is_none() {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": format!(
                        "invalid baseUrl '{}' for git platform '{}': expected an absolute http(s) URL",
                        p.base_url, name
                    )
                })),
            ));
        }
        // Secret-keep matches on the (name, baseUrl) PAIR: pointing an entry
        // at a different instance makes it a different platform as far as
        // credentials are concerned.
        let stored = existing.iter().find(|e| e.name == name && e.base_url == base_url);
        let keep = |submitted: &str, pick: fn(&crate::models::GitPlatformConfig) -> &str| -> String {
            if is_blank_or_masked(submitted) {
                stored.map(|s| pick(s).to_string()).unwrap_or_default()
            } else {
                submitted.to_string()
            }
        };
        let entry = crate::models::GitPlatformConfig {
            name: name.to_string(),
            platform_type,
            base_url,
            token: keep(&p.token, |s| &s.token),
            webhook_secret: keep(&p.webhook_secret, |s| &s.webhook_secret),
            webhook_signing_secret: keep(&p.webhook_signing_secret, |s| &s.webhook_signing_secret),
        };
        match resolved.iter_mut().find(|e| e.name == entry.name) {
            Some(slot) => *slot = entry,
            None => resolved.push(entry),
        }
    }
    Ok(resolved)
}

/// The request-resolved configuration produced by [`apply_ui_config`]: the
/// sets the UI actually submitted, with kept secrets resolved against stored
/// values (the full-replace-with-secret-keep semantics). `put_config`
/// persists THIS to `ui-state.toml` — never the effective runtime state,
/// which may additionally carry env-derived entries (env wins at runtime;
/// env is never persisted, see [`super::persist`]).
#[derive(Debug, Clone)]
pub(crate) struct AppliedConfig {
    /// Request-resolved LLM provider set.
    pub llm: Vec<crate::models::LLMConfig>,
    /// Request-resolved git platform set.
    pub git_platforms: Vec<crate::models::GitPlatformConfig>,
    /// Resolved legacy GitLab fields (post keep/clear/replace semantics).
    pub gitlab_token: String,
    pub gitlab_webhook_secret: String,
    pub gitlab_webhook_signing_secret: String,
    /// The masked UI projection stored in `ui_config`.
    pub ui: UiConfig,
}

/// Apply a UI config payload to the in-memory state — the shared core of
/// `PUT /config` and the `ui-state.toml` startup replay
/// ([`super::persist`]), so hot-apply and cold-start semantics (masked-secret
/// keep, provider rebuild, GitLab runtime sync, gitPlatforms resolution) are
/// identical. Does NOT persist to disk; callers handle that.
///
/// Contract: the payload is a PARTIAL update. Only fields present in the
/// request JSON overwrite the stored config; omitted fields keep their
/// current values. A sparse PUT (e.g. just `{"rules":{"minScore":90}}`)
/// must never silently zero temperature/minScore/maxConcurrentReviews/
/// enableMetrics or drop `llm.providers`/`gitPlatforms`. We merge the
/// request over a snapshot of the stored UI config, then run the save
/// pipeline unchanged — a full-form PUT (every field present) deep-merges to
/// exactly the request, so behaviour is identical to the old wholesale
/// replace.
pub(crate) fn apply_ui_config(
    state: &AppState,
    payload: &serde_json::Value,
) -> Result<AppliedConfig, (StatusCode, Json<serde_json::Value>)> {
    let mut body: UiConfig = {
        let stored = state.ui_config.read().unwrap().clone();
        // UiConfig is a plain struct of serde-native types, so serializing the
        // stored config cannot fail; the fallback is unreachable defensive code.
        let stored_json = serde_json::to_value(&stored).unwrap_or_else(|_| serde_json::json!({}));
        match serde_json::from_value(merge_json(&stored_json, payload)) {
            Ok(ui) => ui,
            Err(e) => {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({"error": format!("invalid config update: {e}")})),
                ));
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

    // Git platforms: resolve the submitted array (full-replace with
    // secret-keep, see `resolve_git_platforms`). Validation only — the
    // resolved set is written to state after the `config not loaded` check
    // below, so a rejected update never partially mutates state.
    let new_platforms = resolve_git_platforms(&body.git_platforms, &state.git_platforms.read().unwrap())?;

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
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "config not loaded"})),
        ));
    }
    drop(cfg_opt);

    // Store the resolved live platform set and project the masked shape into
    // the UI config (which `GET /config` serializes).
    *state.git_platforms.write().unwrap() = new_platforms.clone();
    body.git_platforms = new_platforms
        .iter()
        .map(|p| UiGitPlatformConfig {
            name: p.name.clone(),
            platform_type: p.platform_type.clone(),
            base_url: p.base_url.clone(),
            token: mask_or_empty(&p.token),
            webhook_secret: mask_or_empty(&p.webhook_secret),
            webhook_signing_secret: mask_or_empty(&p.webhook_signing_secret),
        })
        .collect();

    if !new_llm_configs.is_empty() {
        let mut llm = state.llm_configs.write().unwrap();
        *llm = new_llm_configs.clone();
    }

    // Persist full UI config so GET /config returns exactly what was saved
    let mut ui = state.ui_config.write().unwrap();
    *ui = body;

    // Sync GitLab config to the global runtime so webhook handler picks up changes
    // without requiring a restart. The API token follows LLM-key masking
    // semantics (`***` keeps, empty clears, a real value replaces); the real
    // token lives in the runtime only, and `ui_config` persists the mask/empty
    // projection so `GET /config` never echoes it (see `mask_secrets`).
    let (resolved_gitlab_token, resolved_gitlab_webhook_secret, resolved_gitlab_signing_secret) = {
        let rt = crate::server::gitlab::gitlab_runtime();
        let mut gl_rt = rt.write().unwrap();
        let resolved_token = apply_gitlab_runtime_config(&mut gl_rt, &ui.gitlab);
        ui.gitlab.api_token = if resolved_token.is_empty() {
            String::new()
        } else {
            API_KEY_MASK.to_string()
        };
        (
            resolved_token,
            gl_rt.webhook_secret.clone(),
            gl_rt.signing_secret.clone().unwrap_or_default(),
        )
    };

    Ok(AppliedConfig {
        llm: new_llm_configs,
        git_platforms: new_platforms,
        gitlab_token: resolved_gitlab_token,
        gitlab_webhook_secret: resolved_gitlab_webhook_secret,
        gitlab_webhook_signing_secret: resolved_gitlab_signing_secret,
        ui: ui.clone(),
    })
}

pub async fn put_config(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    if !payload.is_object() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "config update must be a JSON object"})),
        )
            .into_response();
    }
    let applied = match apply_ui_config(&state, &payload) {
        Ok(applied) => applied,
        Err((status, body)) => return (status, body).into_response(),
    };

    // Write-through persistence: everything the UI manages (llm, gitlab
    // legacy fields, gitPlatforms, rules, advanced…) lands in
    // `ui-state.toml` so a restart keeps it. The in-memory update above has
    // already been applied either way; a persist failure is surfaced as a
    // 500 so a silently-non-persistent deployment cannot go unnoticed.
    //
    // The file is built from the REQUEST-RESOLVED sets (`applied`), never
    // from the effective runtime state: the runtime may additionally carry
    // env-derived entries (env wins at runtime), and persisting those would
    // leak env secrets to disk and resurrect them on a clean-env restart.
    if let Some(path) = &state.ui_state_path {
        let snapshot = super::persist::UiStateFile::from_applied(&applied, state.ui_state_env.as_ref());
        if let Err(e) = super::persist::save_ui_state(path, &snapshot) {
            tracing::error!(path = %path.display(), error = %e, "failed to persist ui-state.toml");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("config applied in memory but failed to persist to {}: {e}", path.display())
                })),
            )
                .into_response();
        }
    }

    Json(serde_json::json!({"status": "saved"})).into_response()
}
