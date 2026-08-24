//! Persistence of UI-managed configuration to `{config_dir}/ui-state.toml`.
//!
//! `PUT /api/v1/config` hot-applies in memory AND writes this file, so a
//! restart keeps everything the web UI configured (LLM providers, legacy
//! GitLab fields, `gitPlatforms`, rules, advanced…). At server startup the
//! file is loaded and replayed through the SAME code path as `PUT /config`
//! ([`super::put::apply_ui_config`]), so hot-apply and cold-start semantics
//! (masked-secret keep, provider rebuild, GitLab runtime sync) are identical.
//!
//! Precedence: `config.toml` < `ui-state.toml` < env vars — a value supplied
//! via CLI flag / environment always wins over the persisted UI state (see
//! [`UiStateEnvOverrides`]).
//!
//! **This file records UI INTENT, not the effective runtime state.** Values
//! sourced from env/CLI (tracked in [`UiStateEnvOverrides`], consulted at
//! save time by [`UiStateFile::from_applied`]) are never written here:
//! persisting them would both duplicate live secrets at rest in a second
//! location and resurrect env-derived entries on a clean-env restart,
//! changing the provider set the user actually saved.
//!
//! Threat model: secrets the USER saved via the UI (LLM keys, git platform
//! tokens, webhook secrets) are stored PLAINTEXT here — the same threat
//! model as the user-managed `.code-audit-config.toml`. The file is
//! therefore written atomically with `0600` permissions (like `auth.toml`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::models::{GitPlatformConfig, LLMConfig};
use crate::server::AppState;

use super::put::AppliedConfig;
use super::types::{UiConfig, UiGitLabConfig, UiGitPlatformConfig, UiLlmProviderConfig, API_KEY_MASK};

/// File name of the persisted UI state inside the config dir.
pub const UI_STATE_FILE_NAME: &str = "ui-state.toml";

/// On-disk schema of `ui-state.toml`.
///
/// The `ui` section holds the UI projection (secrets masked — only
/// non-secret fields matter there: rules, advanced, URLs, model choices).
/// Live secrets live in the top-level `llm` / `git_platforms` / `gitlab`
/// sections, mirroring their authoritative in-memory stores.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct UiStateFile {
    /// UI projection; `Option` so a hand-written file without `[ui]` does
    /// not replay serde defaults over the startup config.
    #[serde(default)]
    pub ui: Option<UiConfig>,
    /// Live LLM configs (real API keys), as `[[llm]]`.
    #[serde(default)]
    pub llm: Vec<LLMConfig>,
    /// Live git platform entries (real tokens/secrets), as `[[git_platforms]]`.
    #[serde(default)]
    pub git_platforms: Vec<GitPlatformConfig>,
    /// Live legacy GitLab runtime fields.
    #[serde(default)]
    pub gitlab: PersistedGitlabConfig,
}

/// Legacy GitLab credentials as persisted on disk (live values).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PersistedGitlabConfig {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub webhook_secret: String,
    #[serde(default)]
    pub webhook_signing_secret: String,
}

impl UiStateFile {
    /// Build the persistable file from a resolved PUT ([`AppliedConfig`]),
    /// keeping env-derived values OUT: `env` tracks what came from CLI/env
    /// at startup, and any resolved entry/value matching it is skipped.
    ///
    /// Why not snapshot the runtime: the effective runtime (`llm_configs`,
    /// the GitLab runtime global) blends UI-saved config with env-derived
    /// entries. Persisting it would (a) copy env secrets into a second
    /// at-rest location and (b) resurrect env entries on a clean-env
    /// restart — after which `GET /config` would show providers the user
    /// never saved. The resolved PUT set IS the user's intent: entries the
    /// UI submitted, with kept (masked/blank) secrets resolved against
    /// stored values.
    pub(crate) fn from_applied(applied: &AppliedConfig, env: Option<&UiStateEnvOverrides>) -> Self {
        let env_llm: &[LLMConfig] = env.map(|e| e.llm_entries.as_slice()).unwrap_or(&[]);
        let llm: Vec<LLMConfig> = applied
            .llm
            .iter()
            .filter(|c| !is_env_derived_llm(c, env_llm))
            .cloned()
            .collect();

        let mut ui = applied.ui.clone();
        // The persisted ui.llm scalar projection must be consistent with the
        // SAVED provider set, not with whatever env-derived values were
        // seeded into the runtime at startup (the projection survives the
        // replay and feeds `GET /config`).
        sync_llm_projection(&mut ui.llm, &llm);

        // Legacy GitLab scalars: a resolved value identical to the
        // env/CLI-supplied one is not UI intent — persist it as unset.
        let gitlab = PersistedGitlabConfig {
            token: strip_env_value(&applied.gitlab_token, env.and_then(|e| e.gitlab_token.as_ref())),
            webhook_secret: strip_env_value(
                &applied.gitlab_webhook_secret,
                env.and_then(|e| e.gitlab_webhook_secret.as_ref()),
            ),
            webhook_signing_secret: strip_env_value(
                &applied.gitlab_webhook_signing_secret,
                env.and_then(|e| e.gitlab_webhook_signing_secret.as_ref()),
            ),
        };
        // Keep the projection's apiToken marker consistent with what is
        // actually persisted (no "***" for a token the file does not hold).
        ui.gitlab.api_token = if gitlab.token.is_empty() {
            String::new()
        } else {
            API_KEY_MASK.to_string()
        };

        // gitPlatforms have no env/CLI source today, so the resolved set is
        // persisted verbatim; when one is added, skip its entries here
        // exactly like the llm entries above (env is never persisted).
        let git_platforms = applied.git_platforms.clone();

        Self {
            ui: Some(ui),
            llm,
            git_platforms,
            gitlab,
        }
    }
}

/// True when a resolved LLM entry is env-derived: it carries the same
/// secret as an env-seeded entry (provider + non-empty key equality — the
/// leak guard is about the SECRET reaching disk), or, for key-less
/// providers (e.g. local ones), full provider/base/model identity. A user
/// who re-types a DIFFERENT key for the same provider produces a distinct
/// entry that IS persisted.
fn is_env_derived_llm(entry: &LLMConfig, env_entries: &[LLMConfig]) -> bool {
    env_entries.iter().any(|e| {
        e.provider == entry.provider
            && e.api_key == entry.api_key
            && (!e.api_key.is_empty() || (e.api_base == entry.api_base && e.model == entry.model))
    })
}

/// Persist a resolved legacy scalar unless it equals the env/CLI-supplied
/// value — in that case it is env-derived, not UI intent, and is stored as
/// unset. (While the env value is set it wins at load time anyway, so
/// nothing the user sees changes.)
fn strip_env_value(resolved: &str, env: Option<&String>) -> String {
    match env {
        Some(v) if v == resolved => String::new(),
        _ => resolved.to_string(),
    }
}

/// Sync the persisted `ui.llm` scalar projection with the SAVED provider
/// set. The legacy scalar fields (`defaultModel`, `apiBaseUrl`, …) describe
/// the primary provider for old consumers; after an env-seeded entry is
/// skipped they would otherwise still describe that env entry.
fn sync_llm_projection(ui_llm: &mut super::types::UiLlmConfig, saved: &[LLMConfig]) {
    if saved.is_empty() {
        // Nothing persisted (e.g. everything resolved was env-derived): the
        // replay drops the whole llm section when `[[llm]]` is empty, so
        // these scalars never apply — but never record a mask marker for a
        // key the file does not hold.
        ui_llm.openai_api_key = String::new();
        return;
    }
    let primary = saved
        .iter()
        .find(|c| c.provider == ui_llm.primary_provider)
        .unwrap_or(&saved[0]);
    ui_llm.primary_provider = primary.provider.clone();
    ui_llm.default_model = primary.model.clone();
    ui_llm.api_base_url = primary.api_base.clone();
    ui_llm.max_tokens = primary.max_tokens;
    ui_llm.temperature = primary.temperature;
    ui_llm.openai_api_key = if saved.iter().any(|c| c.provider == "openai" && !c.api_key.is_empty()) {
        API_KEY_MASK.to_string()
    } else {
        String::new()
    };
}

/// Where `ui-state.toml` lives: `REVIEW_UI_STATE_FILE` (full path override)
/// wins, then `REVIEW_ENGINE_CONFIG_DIR`, then the default
/// `~/.config/review-engine/` — the same config dir resolution the auth file
/// uses. `None` when no base directory can be determined (persistence
/// disabled, e.g. no home dir).
pub fn resolve_ui_state_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("REVIEW_UI_STATE_FILE") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    let dir = match std::env::var("REVIEW_ENGINE_CONFIG_DIR") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home::home_dir()?.join(".config").join("review-engine"),
    };
    Some(dir.join(UI_STATE_FILE_NAME))
}

/// Persist the UI state atomically (temp file + rename) with `0600`
/// permissions on Unix, so a crash mid-write never leaves a truncated file.
pub fn save_ui_state(path: &Path, state: &UiStateFile) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(state).map_err(std::io::Error::other)?;
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Load the persisted UI state. A missing file yields `Ok(None)`; a corrupt
/// file is an error — the caller logs it and continues startup with
/// config.toml/env values (ignoring it silently would look like "the UI
/// forgot my settings" with no explanation).
pub fn load_ui_state(path: &Path) -> anyhow::Result<Option<UiStateFile>> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            anyhow::bail!("failed to read ui-state file {}: {e}", path.display());
        }
    };
    let parsed: UiStateFile = toml::from_str(&content)
        .map_err(|e| anyhow::anyhow!("failed to parse ui-state file {}: {e}", path.display()))?;
    Ok(Some(parsed))
}

/// Values the environment (CLI flags / env vars) supplied at startup. They
/// win over the persisted file: the replay injects them in place of the
/// file's values (or skips the section), so
/// `config.toml < ui-state.toml < env` holds.
///
/// The same tracking is consulted at SAVE time ([`UiStateFile::from_applied`])
/// to keep env-derived values out of the file: env is never persisted.
#[derive(Debug, Default, Clone)]
pub struct UiStateEnvOverrides {
    /// `--gitlab-token` / `GITLAB_TOKEN`, when set.
    pub gitlab_token: Option<String>,
    /// `--gitlab-webhook-secret` / `GITLAB_WEBHOOK_SECRET`, when set.
    pub gitlab_webhook_secret: Option<String>,
    /// `--gitlab-webhook-signing-secret` / `GITLAB_WEBHOOK_SIGNING_SECRET`, when set.
    pub gitlab_webhook_signing_secret: Option<String>,
    /// True when env seeded the runtime LLM provider list — then the file's
    /// `llm` section is NOT replayed (env wins wholesale for the provider list).
    pub llm_from_env: bool,
    /// The LLM entries env actually seeded at startup (`LLM_CONFIG`, applied
    /// as a fallback only when no config file supplies `[[llm]]`). Save-side
    /// filter: resolved entries matching one of these are env-derived and
    /// are never written to `ui-state.toml`.
    pub llm_entries: Vec<LLMConfig>,
}

/// Load `ui-state.toml` (if present) and apply it to `state` through the
/// same code path as `PUT /config`. Returns `Ok(true)` when a file was
/// applied. Does NOT re-persist: this IS the load path.
pub fn load_and_apply_ui_state(state: &AppState, path: &Path, overrides: &UiStateEnvOverrides) -> anyhow::Result<bool> {
    let Some(file) = load_ui_state(path)? else {
        return Ok(false);
    };
    let payload = replay_payload(&file, overrides);
    super::put::apply_ui_config(state, &payload).map_err(|(status, axum::Json(body))| {
        anyhow::anyhow!("failed to apply {} (HTTP {}): {}", path.display(), status, body)
    })?;
    Ok(true)
}

/// Build the `PUT /config`-equivalent JSON payload from the persisted file,
/// injecting live secrets (the file's `ui` section only carries masks) and
/// applying env precedence.
fn replay_payload(file: &UiStateFile, overrides: &UiStateEnvOverrides) -> serde_json::Value {
    let mut value = match &file.ui {
        Some(ui) => serde_json::to_value(ui).unwrap_or_else(|_| serde_json::json!({})),
        None => serde_json::json!({}),
    };
    let Some(obj) = value.as_object_mut() else {
        return value;
    };

    // Legacy GitLab: env/CLI values win; otherwise the persisted ones. The
    // section is only replayed when some source supplies a credential —
    // otherwise it is dropped so the merge keeps the startup projection and
    // the apply path cannot clear an env-seeded runtime with an empty token.
    let gitlab_token = overrides
        .gitlab_token
        .clone()
        .unwrap_or_else(|| file.gitlab.token.clone());
    let gitlab_webhook_secret = overrides
        .gitlab_webhook_secret
        .clone()
        .unwrap_or_else(|| file.gitlab.webhook_secret.clone());
    let gitlab_signing_secret = overrides
        .gitlab_webhook_signing_secret
        .clone()
        .unwrap_or_else(|| file.gitlab.webhook_signing_secret.clone());
    if !gitlab_token.is_empty() || !gitlab_webhook_secret.is_empty() || !gitlab_signing_secret.is_empty() {
        let mut section: UiGitLabConfig = file.ui.as_ref().map(|u| u.gitlab.clone()).unwrap_or_default();
        section.api_token = gitlab_token;
        section.webhook_secret = gitlab_webhook_secret;
        section.webhook_signing_secret = gitlab_signing_secret;
        if let Ok(v) = serde_json::to_value(&section) {
            obj.insert("gitlab".to_string(), v);
        }
    } else {
        obj.remove("gitlab");
    }

    // LLM: env wins wholesale — drop the section so the merge keeps the
    // startup (env/config-file) projection untouched. Otherwise rebuild the
    // section from the persisted live configs so REAL keys are replayed (a
    // masked key would resolve against the startup configs, which are not
    // the ones the UI saved).
    if overrides.llm_from_env {
        obj.remove("llm");
    } else if !file.llm.is_empty() {
        let mut section = file.ui.as_ref().map(|u| u.llm.clone()).unwrap_or_default();
        section.openai_api_key = file
            .llm
            .iter()
            .find(|c| c.provider == "openai")
            .map(|c| c.api_key.clone())
            .unwrap_or_default();
        section.providers = file
            .llm
            .iter()
            .map(|c| UiLlmProviderConfig {
                provider: c.provider.clone(),
                api_key: c.api_key.clone(),
                api_base_url: c.api_base.clone(),
                default_model: c.model.clone(),
                max_tokens: c.max_tokens,
                temperature: c.temperature,
                timeout_seconds: super::types::default_timeout_seconds(),
                retry_attempts: super::types::default_retry_attempts(),
            })
            .collect();
        if let Ok(v) = serde_json::to_value(&section) {
            obj.insert("llm".to_string(), v);
        }
    } else {
        obj.remove("llm");
    }

    // Git platforms: replay the persisted live entries (real secrets; there
    // is no env source for platforms). Empty stays empty.
    if !file.git_platforms.is_empty() {
        let platforms: Vec<UiGitPlatformConfig> = file
            .git_platforms
            .iter()
            .map(|p| UiGitPlatformConfig {
                name: p.name.clone(),
                platform_type: p.platform_type.clone(),
                base_url: p.base_url.clone(),
                token: p.token.clone(),
                webhook_secret: p.webhook_secret.clone(),
                webhook_signing_secret: p.webhook_signing_secret.clone(),
            })
            .collect();
        if let Ok(v) = serde_json::to_value(&platforms) {
            obj.insert("gitPlatforms".to_string(), v);
        }
    } else {
        obj.remove("gitPlatforms");
    }

    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::api::config::types::API_KEY_MASK;
    use crate::server::AppState;
    use axum::response::IntoResponse;
    use std::sync::Arc;

    use crate::server::gitlab::RUNTIME_TEST_LOCK;

    /// Guard restoring the global GitLab runtime after a test.
    struct RuntimeGuard(crate::server::gitlab::GitLabRuntimeConfig);
    impl RuntimeGuard {
        fn new() -> Self {
            Self(crate::server::gitlab::gitlab_runtime().read().unwrap().clone())
        }
    }
    impl Drop for RuntimeGuard {
        fn drop(&mut self) {
            *crate::server::gitlab::gitlab_runtime().write().unwrap() = self.0.clone();
        }
    }

    fn sample_state_file() -> UiStateFile {
        UiStateFile {
            ui: None,
            llm: vec![LLMConfig {
                provider: "openai".to_string(),
                model: "gpt-4o".to_string(),
                api_key: "sk-live".to_string(),
                api_base: "https://api.openai.com/v1".to_string(),
                max_tokens: 4096,
                temperature: 0.7,
                disable_thinking: None,
            }],
            git_platforms: vec![GitPlatformConfig {
                name: "testbed".to_string(),
                platform_type: "gitlab".to_string(),
                base_url: "http://gitlab.internal:8929".to_string(),
                token: "glpat-platform".to_string(),
                webhook_secret: "wh-secret".to_string(),
                webhook_signing_secret: String::new(),
            }],
            gitlab: PersistedGitlabConfig {
                token: "glpat-legacy".to_string(),
                webhook_secret: "legacy-wh".to_string(),
                webhook_signing_secret: String::new(),
            },
        }
    }

    /// Fresh state seeded the way `serve` seeds it: an `AppConfig` in
    /// `app_config` AND the matching `ui_config` projection (see
    /// `cli/app.rs`), so `apply_ui_config` runs against a realistic store.
    /// Seeding `ui_config` matters when the replay omits a section (env
    /// precedence): the put pipeline then rebuilds from the stored
    /// projection, which must reflect the startup (env/config) values.
    fn fresh_state(llm: Vec<LLMConfig>) -> AppState {
        let mut app: crate::models::AppConfig =
            serde_json::from_value(serde_json::json!({ "llm": [] })).expect("minimal AppConfig must deserialize");
        app.llm = llm.clone();
        let state = AppState::new(llm);
        *state.ui_config.write().unwrap() = crate::server::api::config::UiConfig::from_app_config(&app);
        *state.app_config.write().unwrap() = Some(Arc::new(app));
        state
    }

    #[test]
    fn save_then_load_round_trip_with_live_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(UI_STATE_FILE_NAME);
        let file = sample_state_file();
        save_ui_state(&path, &file).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "ui-state.toml must be 0600, got {mode:o}");
        }

        let loaded = load_ui_state(&path).unwrap().expect("file must load");
        assert_eq!(
            loaded.llm[0].api_key, "sk-live",
            "LLM key persists live (documented threat model)"
        );
        assert_eq!(loaded.git_platforms[0].token, "glpat-platform");
        assert_eq!(loaded.git_platforms[0].base_url, "http://gitlab.internal:8929");
        assert_eq!(loaded.gitlab.token, "glpat-legacy");
        // No tmp file left behind after the atomic rename.
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn load_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_ui_state(&dir.path().join("absent.toml")).unwrap().is_none());
    }

    #[test]
    fn load_corrupt_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        std::fs::write(&path, "this is = not [ valid toml").unwrap();
        assert!(load_ui_state(&path).is_err());
    }

    #[tokio::test]
    async fn restart_equivalent_applies_persisted_state() {
        let _lock = RUNTIME_TEST_LOCK.lock().await;
        let _guard = RuntimeGuard::new();

        // "First boot": a PUT persists everything to a temp ui-state.toml.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        let mut state1 = fresh_state(vec![]);
        state1.ui_state_path = Some(path.clone());
        let state1 = Arc::new(state1);
        let resp = crate::server::api::config::put_config(
            axum::extract::State(state1.clone()),
            axum::Json(serde_json::json!({
                "llm": {
                    "openaiApiKey": "sk-live",
                    "apiBaseUrl": "https://api.openai.com/v1",
                    "defaultModel": "gpt-4o"
                },
                "gitlab": { "apiToken": "glpat-legacy", "webhookSecret": "legacy-wh" },
                "gitPlatforms": [{
                    "name": "testbed",
                    "type": "gitlab",
                    "baseUrl": "http://gitlab.internal:8929",
                    "token": "glpat-platform",
                    "webhookSecret": "wh-secret"
                }]
            })),
        )
        .await
        .into_response();
        assert_eq!(
            resp.status(),
            axum::http::StatusCode::OK,
            "PUT must persist successfully"
        );
        assert!(path.exists(), "ui-state.toml must have been written");

        // "Restart": fresh state, runtime cleared, file loaded and applied.
        *crate::server::gitlab::gitlab_runtime().write().unwrap() = crate::server::gitlab::GitLabRuntimeConfig {
            webhook_secret: String::new(),
            signing_secret: None,
            signing_key: None,
            token: String::new(),
        };
        let state2 = Arc::new(fresh_state(vec![]));
        let applied = load_and_apply_ui_state(&state2, &path, &UiStateEnvOverrides::default()).unwrap();
        assert!(applied);

        // Everything the UI saved is effective again — live secrets in the
        // authoritative stores, masked projections in `ui_config`.
        assert_eq!(state2.llm_configs.read().unwrap()[0].api_key, "sk-live");
        assert_eq!(
            state2.git_platforms.read().unwrap()[0].token,
            "glpat-platform",
            "platform token must survive the restart"
        );
        let rt = crate::server::gitlab::gitlab_runtime().read().unwrap().clone();
        assert_eq!(rt.token, "glpat-legacy");
        assert_eq!(rt.webhook_secret, "legacy-wh");
        let ui = state2.ui_config.read().unwrap().clone();
        assert_eq!(ui.llm.openai_api_key, API_KEY_MASK, "projection stays masked");
        assert_eq!(ui.git_platforms[0].token, API_KEY_MASK);
        assert_eq!(ui.git_platforms[0].webhook_secret, API_KEY_MASK);
        assert!(!ui.git_platforms[0].base_url.is_empty());
    }

    #[tokio::test]
    async fn env_overrides_win_over_persisted_file() {
        let _lock = RUNTIME_TEST_LOCK.lock().await;
        let _guard = RuntimeGuard::new();
        // Startup runtime seeded from env, as `init_gitlab_runtime` does.
        *crate::server::gitlab::gitlab_runtime().write().unwrap() = crate::server::gitlab::GitLabRuntimeConfig {
            webhook_secret: String::new(),
            signing_secret: None,
            signing_key: None,
            token: "glpat-env".to_string(),
        };
        let env_llm = vec![LLMConfig {
            provider: "openai".to_string(),
            model: "gpt-env".to_string(),
            api_key: "sk-env".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            disable_thinking: None,
        }];

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        save_ui_state(&path, &sample_state_file()).unwrap();

        let state = Arc::new(fresh_state(env_llm.clone()));
        let overrides = UiStateEnvOverrides {
            gitlab_token: Some("glpat-env".to_string()),
            llm_from_env: true,
            ..Default::default()
        };
        load_and_apply_ui_state(&state, &path, &overrides).unwrap();

        // Env token wins over the persisted legacy token.
        assert_eq!(
            crate::server::gitlab::gitlab_runtime().read().unwrap().token,
            "glpat-env"
        );
        // Env LLM wins wholesale over the persisted provider list.
        let llm = state.llm_configs.read().unwrap().clone();
        assert_eq!(llm.len(), 1);
        assert_eq!(llm[0].api_key, "sk-env", "persisted sk-live must not override env");
        assert_eq!(llm[0].model, "gpt-env");
        // Platforms have no env source: the persisted entry applies.
        assert_eq!(state.git_platforms.read().unwrap()[0].name, "testbed");
    }

    #[test]
    fn replay_payload_drops_empty_sections() {
        // A file with no credentials at all must produce a payload that
        // cannot clear the startup state.
        let file = UiStateFile::default();
        let payload = replay_payload(&file, &UiStateEnvOverrides::default());
        assert!(
            payload.get("gitlab").is_none(),
            "empty gitlab section must be dropped: {payload}"
        );
        assert!(
            payload.get("llm").is_none(),
            "empty llm section must be dropped: {payload}"
        );
        assert!(payload.get("gitPlatforms").is_none());
    }

    // ── Env-derived values are never persisted (live E2E regression) ──

    /// The env-seeded LLM entry from the E2E repro: provider "openai"
    /// pointing at deepseek, key from env.
    fn env_deepseek_entry() -> LLMConfig {
        LLMConfig {
            provider: "openai".to_string(),
            model: "deepseek-v4-flash".to_string(),
            api_key: "sk-real-deepseek".to_string(),
            api_base: "https://api.deepseek.com".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            disable_thinking: None,
        }
    }

    /// State as the repro's server looks at PUT time: the env-derived entry
    /// is effective in the runtime, and the env tracking knows about it.
    fn state_with_env_llm(path: &Path) -> AppState {
        let mut state = fresh_state(vec![env_deepseek_entry()]);
        state.ui_state_path = Some(path.to_path_buf());
        state.ui_state_env = Some(UiStateEnvOverrides {
            llm_from_env: true,
            llm_entries: vec![env_deepseek_entry()],
            ..Default::default()
        });
        state
    }

    /// The PUT body from the repro: primary "mimo", one mimo provider, and
    /// the masked openai key echoed back by the GET→PUT round trip.
    fn mimo_put_payload() -> serde_json::Value {
        serde_json::json!({
            "llm": {
                "primaryProvider": "mimo",
                "openaiApiKey": API_KEY_MASK,
                "providers": [{
                    "provider": "mimo",
                    "apiKey": "sk-mimo",
                    "apiBaseUrl": "https://token-plan-cn.xiaomimimo.com/v1",
                    "defaultModel": "mimo-v2.5-pro"
                }]
            }
        })
    }

    /// Repro steps 1–3: the env-derived entry stays effective at runtime
    /// (env wins), but the written ui-state.toml must contain ONLY the
    /// user-saved provider — the env secret never reaches disk.
    #[tokio::test]
    async fn put_never_persists_env_derived_llm_entries() {
        let _lock = RUNTIME_TEST_LOCK.lock().await;
        let _guard = RuntimeGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        let state = Arc::new(state_with_env_llm(&path));

        let resp =
            crate::server::api::config::put_config(axum::extract::State(state.clone()), axum::Json(mimo_put_payload()))
                .await
                .into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        // Runtime: env entry remains effective alongside the saved one
        // (env wins at runtime — unchanged behavior).
        let hot = state.llm_configs.read().unwrap().clone();
        assert!(
            hot.iter().any(|c| c.api_key == "sk-real-deepseek"),
            "env entry must stay effective at runtime: {hot:?}"
        );
        assert!(hot.iter().any(|c| c.provider == "mimo"));

        // File: exactly the user-saved set, and the env secret is nowhere.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("sk-real-deepseek"),
            "env secret must never be persisted: {raw}"
        );
        assert!(!raw.contains("deepseek"), "env-derived entry must not appear: {raw}");
        let file = load_ui_state(&path).unwrap().expect("file must load");
        assert_eq!(file.llm.len(), 1, "file must hold exactly the saved set");
        assert_eq!(file.llm[0].provider, "mimo");
        assert_eq!(file.llm[0].api_key, "sk-mimo", "user-saved secrets persist plaintext");

        // The ui.llm scalar projection reflects the saved primary, not the
        // env-derived values.
        let ui = file.ui.expect("ui projection");
        assert_eq!(ui.llm.primary_provider, "mimo");
        assert_eq!(ui.llm.default_model, "mimo-v2.5-pro");
        assert_eq!(ui.llm.api_base_url, "https://token-plan-cn.xiaomimimo.com/v1");
        assert!(
            ui.llm.openai_api_key.is_empty(),
            "no mask marker for a provider the file does not hold"
        );
    }

    /// Repro step 4: restart with a CLEAN env — the persisted set replays
    /// exactly; nothing env-derived resurrects.
    #[tokio::test]
    async fn clean_env_replay_restores_exactly_the_saved_set() {
        let _lock = RUNTIME_TEST_LOCK.lock().await;
        let _guard = RuntimeGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        let state1 = Arc::new(state_with_env_llm(&path));
        let resp = crate::server::api::config::put_config(
            axum::extract::State(state1.clone()),
            axum::Json(mimo_put_payload()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        // Clean-env restart: no env entries, no env overrides.
        let state2 = Arc::new(fresh_state(vec![]));
        let applied = load_and_apply_ui_state(&state2, &path, &UiStateEnvOverrides::default()).unwrap();
        assert!(applied);

        let llm = state2.llm_configs.read().unwrap().clone();
        assert_eq!(llm.len(), 1, "providers must be exactly the saved set: {llm:?}");
        assert_eq!(llm[0].provider, "mimo");
        assert_eq!(llm[0].api_key, "sk-mimo");
        assert_eq!(llm[0].model, "mimo-v2.5-pro");
        assert!(!llm.iter().any(|c| c.api_key == "sk-real-deepseek"));

        let ui = state2.ui_config.read().unwrap().clone();
        assert_eq!(ui.llm.default_model, "mimo-v2.5-pro");
        assert_eq!(ui.llm.providers.len(), 1);
        assert_eq!(ui.llm.providers[0].provider, "mimo");
        assert_eq!(ui.llm.providers[0].api_key, API_KEY_MASK);
    }

    /// A subsequent sparse PUT (no llm section at all) keeps the saved set
    /// in the file — and still never captures the env entry.
    #[tokio::test]
    async fn sparse_put_keeps_saved_set_and_still_skips_env_entries() {
        let _lock = RUNTIME_TEST_LOCK.lock().await;
        let _guard = RuntimeGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        let state = Arc::new(state_with_env_llm(&path));
        let resp =
            crate::server::api::config::put_config(axum::extract::State(state.clone()), axum::Json(mimo_put_payload()))
                .await
                .into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let resp = crate::server::api::config::put_config(
            axum::extract::State(state.clone()),
            axum::Json(serde_json::json!({ "rules": { "minScore": 90 } })),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("sk-real-deepseek"),
            "env secret must never be persisted: {raw}"
        );
        let file = load_ui_state(&path).unwrap().unwrap();
        assert_eq!(file.llm.len(), 1, "sparse PUT must keep the saved set");
        assert_eq!(file.llm[0].provider, "mimo");
        assert_eq!(file.llm[0].api_key, "sk-mimo");
    }

    /// A user re-typing a DIFFERENT key for the env provider's name is UI
    /// intent: the entry is not env-derived and must be persisted.
    #[tokio::test]
    async fn user_key_override_for_env_provider_is_persisted() {
        let _lock = RUNTIME_TEST_LOCK.lock().await;
        let _guard = RuntimeGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        let state = Arc::new(state_with_env_llm(&path));

        let mut payload = mimo_put_payload();
        payload["llm"]["openaiApiKey"] = serde_json::json!("sk-user-typed");
        let resp = crate::server::api::config::put_config(axum::extract::State(state.clone()), axum::Json(payload))
            .await
            .into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let file = load_ui_state(&path).unwrap().unwrap();
        assert!(
            file.llm
                .iter()
                .any(|c| c.provider == "openai" && c.api_key == "sk-user-typed"),
            "a user-typed key is UI intent and must persist: {:?}",
            file.llm
        );
        assert!(file.llm.iter().all(|c| c.api_key != "sk-real-deepseek"));
    }

    /// Legacy GitLab scalars follow the same rule: a resolved value identical
    /// to the env/CLI one is not UI intent and is persisted as unset; a
    /// user-typed value is persisted.
    #[tokio::test]
    async fn put_never_persists_env_gitlab_credentials() {
        let _lock = RUNTIME_TEST_LOCK.lock().await;
        let _guard = RuntimeGuard::new();
        // Startup: env-seeded GitLab runtime (as init_gitlab_runtime does).
        *crate::server::gitlab::gitlab_runtime().write().unwrap() = crate::server::gitlab::GitLabRuntimeConfig {
            webhook_secret: "wh-env".to_string(),
            signing_secret: None,
            signing_key: None,
            token: "glpat-env".to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        let mut state = fresh_state(vec![]);
        state.ui_state_path = Some(path.clone());
        state.ui_state_env = Some(UiStateEnvOverrides {
            gitlab_token: Some("glpat-env".to_string()),
            gitlab_webhook_secret: Some("wh-env".to_string()),
            ..Default::default()
        });
        let state = Arc::new(state);

        // Masked round trip: "***" keeps the (env) runtime token.
        let resp = crate::server::api::config::put_config(
            axum::extract::State(state.clone()),
            axum::Json(serde_json::json!({ "gitlab": { "apiToken": API_KEY_MASK } })),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        // Runtime unchanged (env wins, kept effective).
        assert_eq!(
            crate::server::gitlab::gitlab_runtime().read().unwrap().token,
            "glpat-env"
        );
        // File: no env values at rest.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("glpat-env"), "env token must not be persisted: {raw}");
        assert!(
            !raw.contains("wh-env"),
            "env webhook secret must not be persisted: {raw}"
        );
        let file = load_ui_state(&path).unwrap().unwrap();
        assert!(file.gitlab.token.is_empty());
        assert!(file.gitlab.webhook_secret.is_empty());
        assert!(
            file.ui.unwrap().gitlab.api_token.is_empty(),
            "no mask marker for an unpersisted token"
        );

        // A user-typed token is UI intent and persists.
        let resp = crate::server::api::config::put_config(
            axum::extract::State(state.clone()),
            axum::Json(serde_json::json!({ "gitlab": { "apiToken": "glpat-user" } })),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let file = load_ui_state(&path).unwrap().unwrap();
        assert_eq!(file.gitlab.token, "glpat-user");
    }
}
