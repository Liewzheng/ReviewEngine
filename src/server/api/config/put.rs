use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use base64::Engine;
use std::sync::Arc;

use crate::server::AppState;
use crate::store::traits::ConfigStore;

use super::is_blank_or_masked;
use super::types::{
    default_max_concurrent_reviews, UiConfig, UiGitLabConfig, UiGitPlatformConfig, API_KEY_MASK, CLEAR_SECRET_SENTINEL,
};

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

/// The legacy scalar mirror of the `llm` section (the pre-multi-provider
/// shape: `openaiApiKey` / `apiBaseUrl` / `defaultModel` / …). A `PUT` whose
/// `llm` object carries any of them is a client speaking for the primary
/// through the scalar fields — see [`resolve_llm_section`].
const LEGACY_SCALAR_KEYS: [&str; 7] = [
    "openaiApiKey",
    "apiBaseUrl",
    "defaultModel",
    "maxTokens",
    "temperature",
    "timeoutSeconds",
    "retryAttempts",
];

/// Resolve a `PUT /config` llm section into the provider set to apply.
///
/// `stored` is the effective runtime set: blank/masked secrets of the
/// submission resolve against it ("leave blank = unchanged"). `states_legacy_scalars`
/// tells whether the REQUEST itself stated the legacy scalar mirror (see
/// [`apply_ui_config`]) — the merged body always carries that mirror, because
/// it is part of the stored projection, so only the keys the request actually
/// submitted mark it as an edit a client made.
///
/// Mutates `body.llm` in place: the masked write-back and the normalised
/// primary are part of the projection `GET /config` serves afterwards.
fn resolve_llm_section(
    body: &mut UiConfig,
    stored: &[crate::models::LLMConfig],
    states_legacy_scalars: bool,
) -> Vec<crate::models::LLMConfig> {
    // RENG-75 (identity batch): provider names are display labels, not
    // identities — several cards may share one name, so "keep unchanged"
    // must follow the ENTRY, never the name. Resolution for payload entry
    // `idx` (or a nameless lookup with `None`):
    //   1. the stored entry AT INDEX `idx` matches on the
    //      `(provider, api_base, model)` triple → same card: keep it;
    //   2. otherwise, when EXACTLY ONE stored entry matches the triple → keep
    //      that one (a plain reorder moved the card, its secret follows);
    //   3. otherwise — no match, or several (two same-triple accounts are
    //      indistinguishable in a masked payload) → keep NOTHING. Rule 3 is
    //      what can never mis-assign one account's key to the other; it also
    //      means editing a card's URL or model with a masked key clears the
    //      key (re-enter it).
    let stored_for =
        |idx: Option<usize>, provider: &str, api_base: &str, model: &str| -> Option<crate::models::LLMConfig> {
            let triple =
                |c: &crate::models::LLMConfig| c.provider == provider && c.api_base == api_base && c.model == model;
            if let Some(c) = idx.and_then(|i| stored.get(i)).filter(|c| triple(c)) {
                return Some(c.clone());
            }
            let mut matches = stored.iter().filter(|c| triple(c));
            match (matches.next(), matches.next()) {
                (Some(c), None) => Some(c.clone()),
                _ => None,
            }
        };
    // RENG-75: `disabled` follows the same keep semantics as the masked API
    // key — a providers[] entry that OMITS the key (`None`) keeps the stored
    // flag of the SAME entry, so an unrelated save (or a client that does
    // not know the field yet) cannot silently re-enable a provider the user
    // switched off; an explicit `true`/`false` sets it.
    let resolve_disabled = |submitted: Option<bool>, kept: Option<&crate::models::LLMConfig>| -> bool {
        submitted.or_else(|| kept.map(|c| c.disabled)).unwrap_or(false)
    };
    // RENG-77: `disable_thinking` keeps the same way, but its stored value is
    // itself an `Option` (unset = the flag is not sent to the provider), so
    // the resolution stays tri-state: an omitted key keeps the card's stored
    // `Option` — including "never set" — instead of collapsing it to `false`.
    let resolve_disable_thinking = |submitted: Option<bool>, kept: Option<&crate::models::LLMConfig>| -> Option<bool> {
        submitted.or_else(|| kept.and_then(|c| c.disable_thinking))
    };

    let mut new_llm_configs = Vec::new();

    // The providers[] entry the legacy scalar section consumes when it
    // activates (so the primary is not duplicated in the rebuilt list): the
    // first entry whose triple matches the scalar fields — or, when no entry
    // matches (the scalar fields themselves were edited), the first entry
    // named like the scalar provider, preserving the pre-duplicate round
    // trip. Every OTHER same-named entry is its own card and survives below.
    let legacy_triple_present = body.llm.providers.iter().any(|p| {
        p.provider == "openai" && p.api_base_url == body.llm.api_base_url && p.default_model == body.llm.default_model
    });
    let legacy_consumed: Option<usize> = body.llm.providers.iter().position(|p| {
        p.provider == "openai"
            && (!legacy_triple_present
                || (p.api_base_url == body.llm.api_base_url && p.default_model == body.llm.default_model))
    });

    // Legacy primary (openai): an empty or masked key means "keep the stored
    // key"; a real key replaces it. The stored entry the section is about —
    // the triple the scalar fields describe, else a UNIQUE stored entry named
    // "openai" (two same-named accounts are indistinguishable, so they resolve
    // to nothing rather than to a guessed key) — also supplies every field the
    // submission leaves empty.
    let mut primary_provider: Option<&str> = None;
    let legacy_ref = stored_for(None, "openai", &body.llm.api_base_url, &body.llm.default_model).or_else(|| {
        let mut named = stored.iter().filter(|c| c.provider == "openai");
        match (named.next(), named.next()) {
            (Some(c), None) => Some(c.clone()),
            _ => None,
        }
    });
    let openai_key = if is_blank_or_masked(&body.llm.openai_api_key) {
        legacy_ref.as_ref().map(|c| c.api_key.clone()).unwrap_or_default()
    } else {
        body.llm.openai_api_key.clone()
    };
    // The scalar mirror is a legacy-client affordance for the `openai`-labelled
    // primary, and it may only speak when the REQUEST states it: the merged body
    // always carries the mirror (it is part of the stored projection), so
    // reading an unstated projection back as an edit is what rewrote the whole
    // provider list from an empty mirror — the 0.10.46 release gate's data
    // loss — and what relabelled a same-named account onto another provider's
    // endpoint (measured: an ordinary card edit turned
    // `openai|gpt-4o|api.openai.com` into `openai|deepseek-v4-flash|
    // api.deepseek.com`).
    let mirror_is_the_recorded_primarys_echo = {
        // `sync_llm_projection` fills the mirror from the primary WHATEVER its
        // name, so when the recorded primary is another provider whose own
        // entry carries exactly these base/model values, the mirror describes
        // THAT entry — the field names make no statement about which provider
        // owns it.
        let recorded = body.llm.primary_provider.trim();
        !recorded.is_empty()
            && recorded != "openai"
            && body.llm.providers.iter().any(|p| {
                p.provider == recorded
                    && p.api_base_url.trim() == body.llm.api_base_url.trim()
                    && p.default_model.trim() == body.llm.default_model.trim()
            })
    };
    let legacy_scalars_apply = states_legacy_scalars && !mirror_is_the_recorded_primarys_echo;
    if legacy_scalars_apply && !openai_key.is_empty() {
        primary_provider = Some("openai");
        // Keep-fill: a scalar field left empty states nothing, so the entry's
        // own value comes from the stored entry this section is about. Without
        // it, an empty mirror — a `ui` row written before the mirror existed,
        // replayed back through this path — fabricated an entry with no
        // api_base and no model, unusable, and `POST /reviews` then answered
        // 422 `llmNotConfigured` while the real provider sat in the stored
        // list.
        let api_base = if body.llm.api_base_url.trim().is_empty() {
            legacy_ref.as_ref().map(|c| c.api_base.clone()).unwrap_or_default()
        } else {
            body.llm.api_base_url.clone()
        };
        let model = if body.llm.default_model.trim().is_empty() {
            legacy_ref.as_ref().map(|c| c.model.clone()).unwrap_or_default()
        } else {
            body.llm.default_model.clone()
        };
        // The legacy scalar section has no disabled flag of its own: the
        // providers[] entry it consumes speaks for it, falling back to the
        // stored flag of the entry the scalars describe.
        let disabled = resolve_disabled(
            legacy_consumed.and_then(|i| body.llm.providers[i].disabled),
            legacy_ref.as_ref(),
        );
        // Same source for the RENG-77 opt-out.
        let disable_thinking = resolve_disable_thinking(
            legacy_consumed.and_then(|i| body.llm.providers[i].disable_thinking),
            legacy_ref.as_ref(),
        );
        new_llm_configs.push(crate::models::LLMConfig {
            provider: "openai".to_string(),
            model,
            api_key: openai_key,
            api_base,
            max_tokens: body.llm.max_tokens,
            temperature: body.llm.temperature,
            disable_thinking,
            disabled,
        });
    }

    // Build LLM configs from multi-provider providers Vec. GET /config maps
    // every backend LLM config — including the primary — into `llm.providers`,
    // so a UI round-trip echoes the primary back inside this array. The entry
    // the legacy scalar section consumed is skipped (it is already in the
    // list); every other entry — same-named ones included — is processed by
    // INDEX, never merged by name (RENG-75).
    //
    // `resolved` is aligned with providers[] indices and feeds the masked
    // write-back below: what each submitted entry resolved to (RENG-75's
    // `disabled`, RENG-77's `disable_thinking`).
    let mut resolved: Vec<ResolvedEntry> = Vec::with_capacity(body.llm.providers.len());
    for (i, p) in body.llm.providers.iter().enumerate() {
        if p.provider.is_empty() {
            // A nameless entry names no card to keep from, so an omitted
            // `disable_thinking` stays "never set".
            resolved.push(ResolvedEntry {
                key_present: false,
                disabled: resolve_disabled(p.disabled, None),
                disable_thinking: resolve_disable_thinking(p.disable_thinking, None),
            });
            continue;
        }
        if primary_provider == Some(p.provider.as_str()) && legacy_consumed == Some(i) {
            resolved.push(ResolvedEntry {
                key_present: true,
                disabled: new_llm_configs[0].disabled,
                disable_thinking: new_llm_configs[0].disable_thinking,
            });
            continue;
        }
        // Same "keep unchanged" semantics as the legacy field: a masked key
        // must never overwrite the stored secret with the `***` sentinel —
        // and it must keep THIS entry's secret, not a same-named sibling's.
        let kept = stored_for(Some(i), &p.provider, &p.api_base_url, &p.default_model);
        let key = if is_blank_or_masked(&p.api_key) {
            kept.as_ref().map(|c| c.api_key.clone()).unwrap_or_default()
        } else {
            p.api_key.clone()
        };
        let disabled = resolve_disabled(p.disabled, kept.as_ref());
        // RENG-77: the SAME `kept` card resolves the thinking opt-out, so a
        // card edit that omits the key cannot silently re-enable thinking on
        // the very card it edits.
        let disable_thinking = resolve_disable_thinking(p.disable_thinking, kept.as_ref());
        if key.is_empty() {
            resolved.push(ResolvedEntry {
                key_present: false,
                disabled,
                disable_thinking,
            });
            continue;
        }
        new_llm_configs.push(crate::models::LLMConfig {
            provider: p.provider.clone(),
            model: p.default_model.clone(),
            api_key: key,
            api_base: p.api_base_url.clone(),
            max_tokens: p.max_tokens,
            temperature: p.temperature,
            disable_thinking,
            disabled,
        });
        resolved.push(ResolvedEntry {
            key_present: true,
            disabled,
            disable_thinking,
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
    // The legacy scalar field echoes the PRIMARY provider's key
    // (`UiConfig::from_app_config` fills the scalars from the primary entry,
    // whatever its name), so the mask marker must key off the effective
    // primary — keying it off the literal "openai" would show a configured
    // non-openai primary as "unset" in GET /config after any save.
    let scalar_provider = {
        let p = body.llm.primary_provider.trim();
        if p.is_empty() {
            "openai"
        } else {
            p
        }
    };
    body.llm.openai_api_key = if has_stored_key(scalar_provider) {
        API_KEY_MASK.to_string()
    } else {
        String::new()
    };
    for (i, p) in body.llm.providers.iter_mut().enumerate() {
        let entry = resolved.get(i).copied().unwrap_or_default();
        p.api_key = if entry.key_present {
            API_KEY_MASK.to_string()
        } else {
            String::new()
        };
        // Store the RESOLVED flag (keep semantics applied) so `GET /config`
        // always reports a concrete bool and the next merge starts from it.
        p.disabled = Some(entry.disabled);
        // RENG-77: same rule for the thinking opt-out — the resolved
        // (post-keep) value is what the projection echoes, so the next merge
        // starts from what was actually applied. `None` stays `None` (the
        // flag is not sent to the provider), never a fabricated `false`.
        p.disable_thinking = entry.disable_thinking;
    }

    // RENG-75: the effective head is always an ENABLED provider. A recorded
    // primary that still names an enabled entry is kept verbatim — RENG-72: a
    // save that does not speak for the primary must never change it. One that
    // is empty, unmatched, or names a now-DISABLED entry is normalised to the
    // first enabled provider (or emptied when none are enabled — the stored
    // order then stays authoritative until an entry is re-enabled).
    if !new_llm_configs.is_empty()
        && !new_llm_configs
            .iter()
            .any(|c| !c.disabled && c.provider == body.llm.primary_provider)
    {
        body.llm.primary_provider = new_llm_configs
            .iter()
            .find(|c| !c.disabled)
            .map(|c| c.provider.clone())
            .unwrap_or_default();
    }

    new_llm_configs
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

/// Normalise a submitted `allowedProjects` list: trim every entry, drop
/// blank/whitespace-only entries, and de-duplicate (first occurrence wins,
/// order preserved). No format validation — any non-empty string is kept.
fn sanitize_allowed_projects(allowed: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    allowed
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .filter(|s| seen.insert(s.clone()))
        .collect()
}

/// True when `raw` is an absolute http(s) URL carrying a non-empty authority.
///
/// Shared by the `baseUrl` and `internalBaseUrl` checks. The authority check
/// closes a `url`-crate quirk: `http:///host` parses successfully with host
/// "host" (the empty authority is collapsed), so the original string must
/// carry a non-empty authority between `scheme://` and the next `/`.
fn is_absolute_http_url(raw: &str) -> bool {
    reqwest::Url::parse(raw)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
        .filter(|_| {
            raw.split_once("://")
                .is_some_and(|(_, rest)| !rest.is_empty() && !rest.starts_with('/'))
        })
        .is_some()
}

/// Resolve the submitted `gitPlatforms` array into the new live set.
///
/// Semantics: when the `gitPlatforms` key is present, the submitted array
/// REPLACES the full configured set (to delete an entry, submit the array
/// without it). Each entry's secrets resolve as:
/// - a real value replaces the stored secret;
/// - blank (`""`) or the mask sentinel (`***`) KEEPS the stored secret of
///   the entry this submission is about (the UI shows the mask for a
///   configured secret and submits it unchanged);
/// - the clear sentinel ([`CLEAR_SECRET_SENTINEL`]) explicitly CLEARS it —
///   the one affordance "blank = keep" leaves no room for.
///
/// Identity (RENG-96): which stored entry a submission is about is decided
/// by the entry's stable `id`. A well-formed submitted id is the entry's
/// identity and is ALWAYS kept — even when no stored entry carries it yet
/// (a cold-start replay re-feeds the persisted ids into an empty store;
/// re-minting there would make ids drift on every restart). `baseUrl` is
/// NEVER part of the match — it is an editable address, so repointing an
/// entry keeps its credentials (a same-named entry pointing at a different
/// instance is a DIFFERENT entry exactly when the payload says so:
/// different id or, for legacy clients, a name with no stored match).
/// Secrets keep from the id-matched stored entry, else the name-matched
/// one, else nothing. An absent or malformed id falls back to the legacy
/// `name` matching (pre-RENG-96 clients), adopting the matched stored
/// entry's id; a truly new entry gets a freshly generated id — the ONLY
/// case an id is ever minted. Existing databases keep working: a row whose
/// id was never threaded through (legacy rows/files) gets one on the next
/// write, and its credentials survive that write because the name fallback
/// resolves them before the fresh id is assigned. Duplicate submission
/// identities in one array: last write wins.
fn resolve_git_platforms(
    submitted: &[UiGitPlatformConfig],
    existing: &[crate::models::GitPlatformConfig],
) -> Result<Vec<crate::models::GitPlatformConfig>, (StatusCode, Json<serde_json::Value>)> {
    let mut resolved: Vec<crate::models::GitPlatformConfig> = Vec::new();
    // Submission identity per resolved entry: `(has_id, key)` — the id when
    // the payload carried one, else the name (legacy clients).
    let mut identities: Vec<(bool, String)> = Vec::new();
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
        // baseUrl keys the credential routing (host:port matching at
        // review/webhook time), so an empty, unparseable, or non-http(s)
        // value is a hard 422 — never a silently stored broken entry. It is
        // NOT part of the secret-keep identity (see below).
        if !is_absolute_http_url(&base_url) {
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
        // internalBaseUrl is optional (empty = unconfigured, review pulls fall
        // back to baseUrl) and validated with the same rule as baseUrl when
        // non-empty: a non-http(s) / unparseable value is a hard 422. It is
        // NOT part of the secret-keep match key below — changing it does not
        // change which instance an entry is (identity keys off `id`), so
        // credentials carry over across an internalBaseUrl edit.
        let internal_base_url = p.internal_base_url.trim().trim_end_matches('/').to_string();
        if !internal_base_url.is_empty() && !is_absolute_http_url(&internal_base_url) {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": format!(
                        "invalid internalBaseUrl '{}' for git platform '{}': expected an absolute http(s) URL",
                        p.internal_base_url, name
                    )
                })),
            ));
        }
        // Secret-keep identity (RENG-96): the entry's stable `id`. A
        // well-formed submitted id IS the entry's identity — it is kept even
        // when this session's store cannot look it up, because a cold-start
        // replay re-feeds the persisted ids into an empty store and
        // re-minting would make the id drift on every restart (the one
        // remaining way a rename + restart could lose credentials). Secrets
        // keep from the id-matched stored entry, else the name-matched one,
        // else nothing. An absent or malformed id falls back to the legacy
        // `name` matching (pre-RENG-96 clients), adopting the matched stored
        // entry's id; a genuinely new entry gets a freshly generated id.
        let id_raw = p.id.trim();
        let id_is_well_formed = uuid::Uuid::parse_str(id_raw).is_ok();
        let stored = if id_is_well_formed {
            existing
                .iter()
                .find(|e| e.id == id_raw)
                .or_else(|| existing.iter().find(|e| e.name == name))
        } else {
            existing.iter().find(|e| e.name == name)
        };
        let id = if id_is_well_formed {
            id_raw.to_string()
        } else {
            // Absent or malformed: adopt the name-matched entry's id (legacy
            // round trip) or mint one for a genuinely new entry.
            stored
                .map(|s| s.id.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
        };
        let keep = |submitted: &str, pick: fn(&crate::models::GitPlatformConfig) -> &str| -> String {
            if submitted == CLEAR_SECRET_SENTINEL {
                // The UI's explicit clear affordance; blank/mask mean "keep".
                String::new()
            } else if is_blank_or_masked(submitted) {
                stored.map(|s| pick(s).to_string()).unwrap_or_default()
            } else {
                submitted.to_string()
            }
        };
        let entry = crate::models::GitPlatformConfig {
            id,
            name: name.to_string(),
            platform_type,
            base_url,
            internal_base_url,
            token: keep(&p.token, |s| &s.token),
            webhook_secret: keep(&p.webhook_secret, |s| &s.webhook_secret),
            webhook_signing_secret: keep(&p.webhook_signing_secret, |s| &s.webhook_signing_secret),
            allowed_projects: sanitize_allowed_projects(&p.allowed_projects),
        };
        // Dedupe by the identity the SUBMISSION carries — the id when set,
        // the name for pre-RENG-96 (id-less) clients. Two id-carrying
        // entries may share a name (distinct entries); two id-less entries
        // with the same name are the same legacy submission. Last write
        // wins, as before.
        let sub_key = if p.id.is_empty() {
            (false, name.to_string())
        } else {
            (true, p.id.clone())
        };
        match identities.iter().position(|k| k == &sub_key) {
            Some(i) => resolved[i] = entry,
            None => {
                identities.push(sub_key);
                resolved.push(entry);
            }
        }
    }
    Ok(resolved)
}

/// What one submitted `llm.providers[]` entry resolved to, aligned with the
/// entry's index. Drives the masked/keep write-back in [`apply_ui_config`]:
/// `key_present` is the masked API-key echo, and the two flags are the
/// post-keep values `GET /config` reports (RENG-75's `disabled`, RENG-77's
/// `disable_thinking`).
#[derive(Debug, Clone, Copy, Default)]
struct ResolvedEntry {
    key_present: bool,
    disabled: bool,
    disable_thinking: Option<bool>,
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
    // ── The llm provider SET is re-derived only when this request states it ──
    //
    // `PUT /config` is a partial update, and the provider set has exactly two
    // representations: a stated `providers` array and the legacy scalar mirror.
    // An `llm` member that states neither (`{"llm":{}}`, or a lone
    // `primaryProvider`) patches the projection but says nothing about the set,
    // so the stored set is kept verbatim.
    //
    // Re-deriving from anything else is data loss, and both directions have
    // been measured. The Configuration page's auto-save omits `llm` entirely
    // (LLM settings are managed on the /llm page — see `useConfigForm`), and
    // resolving the set anyway applied the merged projection's legacy mirror —
    // EMPTY on any `ui` row written before the mirror existed — as the primary
    // instead of `providers[0]`, replacing the list with one entry carrying no
    // api_base (the 0.10.46 release gate: `GET /llm/providers` unusable, every
    // `POST /reviews` 422 `llmNotConfigured`). The projection's `providers[]`
    // is not authoritative either — `sync_llm_projection` keeps the scalar
    // mirror in sync and never the array, so a scan that configures a provider
    // through the legacy scalars only leaves the array empty while the table
    // holds the row, and re-deriving from the empty array deleted the provider
    // on a 200-OK save (visible only after the next boot).
    let submitted_llm = payload.get("llm").and_then(serde_json::Value::as_object);
    // Does the REQUEST state the legacy scalar mirror? The merged body always
    // carries it (it is part of the stored projection), so only a key on the
    // submitted `llm` object marks it as an edit a client actually made — see
    // [`resolve_llm_section`].
    let states_legacy_scalars =
        submitted_llm.is_some_and(|llm| LEGACY_SCALAR_KEYS.iter().any(|key| llm.contains_key(*key)));
    // A stated `providers` array REPLACES the set (an empty array is the
    // explicit "every provider was deleted" the /llm page sends); a stated
    // legacy scalar describes the set through the legacy path.
    let states_the_provider_set =
        submitted_llm.is_some_and(|llm| llm.contains_key("providers")) || states_legacy_scalars;
    let new_llm_configs = if states_the_provider_set {
        resolve_llm_section(&mut body, &existing_llm, states_legacy_scalars)
    } else {
        // Persistence keeps the stored set; the runtime set and the projection
        // are left exactly as they are, so this save cannot touch a provider.
        existing_llm.clone()
    };

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
        // RENG-107: the two caps the review pipeline enforces, set together
        // exactly as the DB overlay sets them.
        //
        // A value of 0 — what a payload, or a STORED ui row, yields when its
        // `advanced` section is absent (serde then fills the derived
        // `UiAdvancedConfig::default()`) — is not a decision: the pipeline
        // builds a 0-permit `Semaphore` from these caps, so every expert task
        // would wait forever instead of running, and the startup replay would
        // re-apply that zero on every boot. So 0 means "this update did not
        // decide the caps": the running values stay, and the projection is
        // published with the value actually in force, so the Configuration page
        // neither shows a cap that cannot apply nor persists it again (this
        // heals a legacy row on the next save).
        let submitted_caps = body.advanced.max_concurrent_reviews as usize;
        if submitted_caps > 0 {
            new_cfg.max_concurrent_llm_calls = Some(submitted_caps);
            new_cfg.max_team_size = Some(submitted_caps);
        } else {
            body.advanced.max_concurrent_reviews = new_cfg
                .max_concurrent_llm_calls
                .unwrap_or(default_max_concurrent_reviews() as usize)
                as u32;
        }
        // RENG-95: the aggregation flag is part of the UI projection, so a
        // `PUT /config` that mentions it applies it here (the config page never
        // sends it — it is controlled from the experts page — and a save that
        // omits it keeps the merged stored value, see the `Option`). The
        // startup replay lands here too, which is what restores a persisted
        // toggle after a restart.
        if let Some(aggregated) = body.aggregated {
            new_cfg.report.aggregated = aggregated;
        }
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

    // RENG-97: a cached probe outcome describes the entry it probed — a
    // removed entry must not keep serving its old verdict (a changed token is
    // already invalidated at lookup time by the fingerprint). The next read of
    // the dashboard / `/system/health` reports the removed entry as
    // never-probed again instead of the pre-change status.
    {
        let dropped = state.git_health.retain_live(&new_platforms);
        if dropped > 0 {
            tracing::debug!(
                dropped,
                "dropped cached git platform health for removed entries (RENG-97)"
            );
        }
    }

    body.git_platforms = new_platforms
        .iter()
        .map(|p| UiGitPlatformConfig {
            id: p.id.clone(),
            name: p.name.clone(),
            platform_type: p.platform_type.clone(),
            base_url: p.base_url.clone(),
            internal_base_url: p.internal_base_url.clone(),
            token: mask_or_empty(&p.token),
            webhook_secret: mask_or_empty(&p.webhook_secret),
            webhook_signing_secret: mask_or_empty(&p.webhook_signing_secret),
            allowed_projects: p.allowed_projects.clone(),
        })
        .collect();

    if !new_llm_configs.is_empty() {
        let mut llm = state.llm_configs.write().unwrap();
        *llm = new_llm_configs.clone();
    }

    // RENG-36: a cached health entry is only valid for the exact config it was
    // probed with, so drop every entry whose provider is no longer in the new
    // effective set — edited credentials, a cleared key (which removes the
    // provider), a renamed provider, an edited `apiBase`. The next read of
    // `GET /llm/providers` / the dashboard probes the changed provider again
    // instead of reporting the pre-change status.
    {
        let live = state.llm_configs.read().unwrap();
        let dropped = state.llm_health.retain_live(&live);
        if dropped > 0 {
            tracing::debug!(dropped, "dropped cached LLM health for changed providers (RENG-36)");
        }
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
    // legacy fields, gitPlatforms, rules, advanced…) is persisted so a
    // restart keeps it — to the database when `state.db` is set (0.10.0,
    // §6.2), otherwise to `ui-state.toml` (0.9 behaviour, also the
    // REVIEW_DISABLE_DB escape hatch). The in-memory update above has
    // already been applied either way; a persist failure is surfaced as a
    // 500 so a silently-non-persistent deployment cannot go unnoticed.
    //
    // The snapshot is built from the REQUEST-RESOLVED sets (`applied`), never
    // from the effective runtime state: the runtime may additionally carry
    // env-derived entries (env wins at runtime), and persisting those would
    // leak env secrets to disk and resurrect them on a clean-env restart.
    // The env filter (`UiStateFile::from_applied`) is identical for both
    // sinks: env/CLI values are never persisted anywhere.
    if let Some(db) = &state.db {
        let snapshot = super::persist::UiStateFile::from_applied(&applied, state.ui_state_env.as_ref());
        if let Err(e) = db.save_ui_state(&snapshot).await {
            tracing::error!(error = %e, "failed to persist config to the database");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("config applied in memory but failed to persist to the database: {e}")
                })),
            )
                .into_response();
        }
    } else if let Some(path) = &state.ui_state_path {
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
