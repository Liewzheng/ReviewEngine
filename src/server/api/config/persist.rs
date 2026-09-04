//! Persistence of UI-managed configuration to `{config_dir}/ui-state.toml`.
//!
//! `PUT /api/v1/config` hot-applies in memory AND writes this file, so a
//! restart keeps everything the web UI configured (LLM providers, legacy
//! GitLab fields, `gitPlatforms`, rules, advanced…). At server startup the
//! file is loaded and replayed through the SAME code path as `PUT /config`
//! ([`super::put::apply_ui_config`]), so hot-apply and cold-start semantics
//! (masked-secret keep, provider rebuild, GitLab runtime sync) are identical.
//!
//! Precedence for gitlab credentials: `env/CLI < ui-state.toml` — the
//! persisted file is the authoritative source; env vars / CLI flags are
//! fallback-only, used just when the file's value is empty, and each such
//! use logs a deprecation warning (configure the credential in the Web UI
//! instead). For the LLM provider list env still wins wholesale:
//! `config.toml < ui-state.toml < env` (see [`UiStateEnvOverrides`]).
//!
//! **This file records UI INTENT, not the effective runtime state.** Values
//! sourced from env/CLI (tracked in [`UiStateEnvOverrides`], consulted at
//! save time by [`UiStateFile::from_applied`]) are never written here:
//! persisting them would both duplicate live secrets at rest in a second
//! location and resurrect env-derived entries on a clean-env restart,
//! changing the provider set the user actually saved.
//!
//! Threat model: git credentials the USER saved via the UI (git platform
//! tokens, webhook secrets, legacy GitLab fields) are encrypted at rest with
//! a per-config-dir local key (`secrets.key`, 32 random bytes, `0600`); only
//! the persistence boundary encrypts/decrypts, the in-memory/runtime path
//! stays plaintext. LLM API keys remain plaintext at rest (outside the scope
//! of the encrypted-secrets change). The file and the key are written
//! atomically with `0600` permissions on Unix.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::config::secrets::{self, ENC_PREFIX};
use crate::models::{GitPlatformConfig, LLMConfig};
use crate::server::AppState;
use crate::store::traits::ConfigStore;
use crate::store::SqlxStore;

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
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
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

/// True when a resolved LLM entry is env-derived. The leak guard is about
/// the SECRET reaching disk, so any resolved entry carrying an env entry's
/// live key is env-derived when it is recognizably the same credential:
/// same provider label (the normal reconstruction: a masked round trip
/// resolves the env key back under its own name), or the same endpoint
/// (a reconstruction path that relabeled the provider still carries the
/// secret + base — matching on the label alone would let it through). For
/// key-less providers (e.g. local ones) there is no secret to leak, so
/// identity requires full provider/base/model equality. A user who re-types
/// a DIFFERENT key for the same provider produces a distinct entry that IS
/// persisted.
fn is_env_derived_llm(entry: &LLMConfig, env_entries: &[LLMConfig]) -> bool {
    env_entries.iter().any(|e| {
        if e.api_key.is_empty() {
            entry.api_key.is_empty()
                && e.provider == entry.provider
                && e.api_base == entry.api_base
                && e.model == entry.model
        } else {
            e.api_key == entry.api_key && (e.provider == entry.provider || e.api_base == entry.api_base)
        }
    })
}

/// Persist a resolved legacy scalar unless it equals the env/CLI-supplied
/// value — in that case it is env-derived, not UI intent, and is stored as
/// unset. (At load time the env value is only a fallback for an empty file
/// value, so the same credential still lands in the runtime — nothing the
/// user sees changes.)
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

/// At-rest form of a [`UiStateFile`]: git platform and legacy GitLab secrets
/// are encrypted with the local key (empty values stay empty). LLM API keys
/// and the masked `ui` projection are written as-is — they are outside the
/// scope of the encrypted-secrets change.
fn encrypt_ui_state(state: &UiStateFile, key: &[u8; 32]) -> anyhow::Result<UiStateFile> {
    let mut out = state.clone();
    for p in &mut out.git_platforms {
        p.token = encrypt_non_empty(&p.token, key)?;
        p.webhook_secret = encrypt_non_empty(&p.webhook_secret, key)?;
        p.webhook_signing_secret = encrypt_non_empty(&p.webhook_signing_secret, key)?;
    }
    out.gitlab.token = encrypt_non_empty(&out.gitlab.token, key)?;
    out.gitlab.webhook_secret = encrypt_non_empty(&out.gitlab.webhook_secret, key)?;
    out.gitlab.webhook_signing_secret = encrypt_non_empty(&out.gitlab.webhook_signing_secret, key)?;
    Ok(out)
}

fn encrypt_non_empty(value: &str, key: &[u8; 32]) -> anyhow::Result<String> {
    if value.is_empty() {
        Ok(String::new())
    } else {
        secrets::encrypt_secret(value, key)
    }
}

/// Undo [`encrypt_ui_state`] in place. Non-`enc:` values (legacy plaintext)
/// pass through unchanged via [`secrets::decrypt_secret`].
fn decrypt_ui_state(state: &mut UiStateFile, key: &[u8; 32]) -> anyhow::Result<()> {
    for p in &mut state.git_platforms {
        p.token = secrets::decrypt_secret(&p.token, key)?;
        p.webhook_secret = secrets::decrypt_secret(&p.webhook_secret, key)?;
        p.webhook_signing_secret = secrets::decrypt_secret(&p.webhook_signing_secret, key)?;
    }
    state.gitlab.token = secrets::decrypt_secret(&state.gitlab.token, key)?;
    state.gitlab.webhook_secret = secrets::decrypt_secret(&state.gitlab.webhook_secret, key)?;
    state.gitlab.webhook_signing_secret = secrets::decrypt_secret(&state.gitlab.webhook_signing_secret, key)?;
    Ok(())
}

/// True when the file carries at least one `enc:` value, i.e. decryption is
/// required at load time. A legacy all-plaintext file needs no key at all.
fn has_encrypted_secrets(state: &UiStateFile) -> bool {
    state.git_platforms.iter().any(|p| {
        p.token.starts_with(ENC_PREFIX)
            || p.webhook_secret.starts_with(ENC_PREFIX)
            || p.webhook_signing_secret.starts_with(ENC_PREFIX)
    }) || state.gitlab.token.starts_with(ENC_PREFIX)
        || state.gitlab.webhook_secret.starts_with(ENC_PREFIX)
        || state.gitlab.webhook_signing_secret.starts_with(ENC_PREFIX)
}

/// Persist the UI state atomically (temp file + rename) with `0600`
/// permissions on Unix, so a crash mid-write never leaves a truncated file.
/// Git credentials are encrypted at rest with the local `secrets.key`
/// (auto-created on first save); the in-memory [`UiStateFile`] is untouched.
pub fn save_ui_state(path: &Path, state: &UiStateFile) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let key_path = secrets::key_path_for(path);
    let key = secrets::load_or_create_key(&key_path)?;
    let encrypted = encrypt_ui_state(state, &key)?;
    let content = toml::to_string_pretty(&encrypted)?;
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
///
/// `enc:` values are decrypted back to plaintext so the replay path sees the
/// same in-memory shape as before. A legacy all-plaintext file needs no key
/// and loads as-is. An `enc:` value with no key file (or a decrypt failure)
/// is a hard error naming `secrets.key` — never a panic, never silent.
pub fn load_ui_state(path: &Path) -> anyhow::Result<Option<UiStateFile>> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            anyhow::bail!("failed to read ui-state file {}: {e}", path.display());
        }
    };
    let mut parsed: UiStateFile = toml::from_str(&content)
        .map_err(|e| anyhow::anyhow!("failed to parse ui-state file {}: {e}", path.display()))?;
    if has_encrypted_secrets(&parsed) {
        let key_path = secrets::key_path_for(path);
        let key = secrets::load_key(&key_path)?.ok_or_else(|| {
            anyhow::anyhow!(
                "{} holds encrypted secrets but the decryption key {} is missing; \
                 re-enter the secrets in the web UI and save again to restore them",
                path.display(),
                key_path.display()
            )
        })?;
        decrypt_ui_state(&mut parsed, &key)
            .map_err(|e| anyhow::anyhow!("failed to decrypt secrets in {}: {e}", path.display()))?;
    }
    Ok(Some(parsed))
}

/// Values the environment (CLI flags / env vars) supplied at startup. Since
/// the persistence-file priority inversion these are FALLBACK-ONLY for the
/// gitlab legacy credentials: the replay uses them only when the file's
/// value is empty (logging a deprecation warning) — the file's values are
/// authoritative. `llm_from_env` still wins wholesale for the LLM provider
/// list, where `config.toml < ui-state.toml < env` holds.
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
    apply_replay(state, &file, overrides, &path.display().to_string())?;
    Ok(true)
}

/// Shared tail of both replay paths: build the PUT-equivalent payload and
/// push it through `apply_ui_config`. `source_desc` only feeds error text.
fn apply_replay(
    state: &AppState,
    file: &UiStateFile,
    overrides: &UiStateEnvOverrides,
    source_desc: &str,
) -> anyhow::Result<()> {
    let payload = replay_payload(file, overrides);
    super::put::apply_ui_config(state, &payload).map_err(|(status, axum::Json(body))| {
        anyhow::anyhow!("failed to apply {source_desc} (HTTP {status}): {body}")
    })?;
    Ok(())
}

// ── 0.10.0 database-backed persistence (design/persistence.md §6) ──

/// Suffix of the post-import backup: `ui-state.toml` →
/// `ui-state.toml.migrated` (kept, never deleted — §6.1).
pub const MIGRATED_SUFFIX: &str = ".migrated";

/// `ui-state.toml` → `ui-state.toml.migrated`.
pub fn migrated_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_owned();
    os.push(MIGRATED_SUFFIX);
    PathBuf::from(os)
}

/// True when `REVIEW_DISABLE_DB` requests the 0.9 escape hatch (§9).
/// Pure function over the env value for testability.
pub fn db_disabled_flag(value: Option<&str>) -> bool {
    matches!(
        value.map(|v| v.trim().to_ascii_lowercase()),
        Some(v) if v == "1" || v == "true" || v == "yes"
    )
}

/// Startup step 1 (§6.1): resolve the DB URL, build the pool, run
/// migrations. `Ok(None)` = persistence disabled (escape hatch, or no
/// config dir resolvable — the 0.9 "persistence off" case); an unreachable
/// database is a hard startup error, NEVER a silent SQLite fallback (§9).
pub async fn bootstrap_database() -> anyhow::Result<Option<SqlxStore>> {
    if db_disabled_flag(std::env::var("REVIEW_DISABLE_DB").ok().as_deref()) {
        tracing::warn!("persistence disabled via REVIEW_DISABLE_DB — running with 0.9 in-memory + file behaviour");
        return Ok(None);
    }
    let store = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => SqlxStore::connect(&url).await.with_context(|| {
            "DATABASE_URL is set but the database is unreachable; \
             refusing to silently fall back to embedded SQLite (fix the connection, \
             unset DATABASE_URL, or set REVIEW_DISABLE_DB=1 to bypass persistence)"
                .to_string()
        })?,
        _ => {
            let Some(state_path) = resolve_ui_state_path() else {
                tracing::warn!("no config dir resolvable — persistence disabled (0.9 behaviour)");
                return Ok(None);
            };
            let dir = state_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            SqlxStore::connect_default(&dir).await?
        }
    };
    store.migrate().await?;
    Ok(Some(store))
}

/// Startup step 3 (§6.1): one-shot import of `ui-state.toml` into the DB.
///
/// Triggers only when the three config tables are completely empty AND the
/// file exists. The whole import is ONE transaction (`save_ui_state`), so a
/// mid-import failure rolls back cleanly; the file is renamed to
/// `.migrated` only after every table has been written. Any error leaves
/// the file untouched so the caller falls back to the file replay path and
/// the next startup retries the import.
pub async fn import_ui_state_into_db(store: &SqlxStore, path: &Path) -> anyhow::Result<bool> {
    if !store.config_tables_empty().await? {
        return Ok(false);
    }
    let Some(file) = load_ui_state(path)? else {
        return Ok(false);
    };
    store.save_ui_state(&file).await?;
    match std::fs::rename(path, migrated_path(path)) {
        Ok(()) => tracing::info!(
            "imported ui-state.toml into the database; backup at {}",
            migrated_path(path).display()
        ),
        // The data is safely in the DB and DB replay wins from here on; a
        // failed rename only means the backup was not created.
        Err(e) => tracing::warn!("import succeeded but renaming {} failed: {e}", path.display()),
    }
    Ok(true)
}

/// Startup step 4 (§6.1): replay the UI state from the DB through the SAME
/// `apply_ui_config` path as `PUT /config`. The DB rows are reassembled into
/// a [`UiStateFile`] so the replay payload builder (env precedence, masked
/// projections) is literally shared with the file path. `Ok(false)` = DB
/// holds no configuration at all (caller falls back to the file replay).
pub async fn load_and_apply_ui_state_from_db(
    state: &AppState,
    store: &SqlxStore,
    overrides: &UiStateEnvOverrides,
) -> anyhow::Result<bool> {
    let ui: Option<UiConfig> = store
        .load_setting("ui")
        .await?
        .map(|v| serde_json::from_value(v).context("app_settings row 'ui' is not a valid UiConfig"))
        .transpose()?;
    let file = UiStateFile {
        ui,
        llm: store.load_llm_providers().await?,
        git_platforms: store.load_git_platforms().await?,
        gitlab: store.load_legacy_gitlab().await?,
    };
    let gitlab = &file.gitlab;
    let empty = file.ui.is_none()
        && file.llm.is_empty()
        && file.git_platforms.is_empty()
        && gitlab.token.is_empty()
        && gitlab.webhook_secret.is_empty()
        && gitlab.webhook_signing_secret.is_empty();
    if empty {
        return Ok(false);
    }
    apply_replay(state, &file, overrides, "the database UI state")?;
    Ok(true)
}

/// Build the `PUT /config`-equivalent JSON payload from the persisted file,
/// injecting live secrets (the file's `ui` section only carries masks) and
/// applying env/CLI values as a DEPRECATED fallback only where the file's
/// gitlab values are empty.
fn replay_payload(file: &UiStateFile, overrides: &UiStateEnvOverrides) -> serde_json::Value {
    let mut value = match &file.ui {
        Some(ui) => serde_json::to_value(ui).unwrap_or_else(|_| serde_json::json!({})),
        None => serde_json::json!({}),
    };
    let Some(obj) = value.as_object_mut() else {
        return value;
    };

    // Legacy GitLab: the persisted file is now the AUTHORITATIVE source —
    // the Web UI / ui-state.toml is the single source of truth for webhook
    // verification and dispatch config. env/CLI values are FALLBACK-ONLY:
    // used only when the file's value is empty, and every such use is
    // DEPRECATED — log a warning telling the user to configure the
    // credential in the Web UI instead. The section is still only replayed
    // when some source supplies a credential; otherwise it is dropped so the
    // merge keeps the startup projection and the apply path cannot clear the
    // runtime with an empty token.
    fn env_fallback(value: &str, env: &Option<String>, field: &str) -> String {
        if !value.is_empty() {
            return value.to_string();
        }
        match env {
            Some(v) => {
                tracing::warn!(
                    "gitlab {field} from env/CLI is deprecated: configure it in the Web UI \
                     (the persisted ui-state.toml is the authoritative source for gitlab \
                     credentials; file values take precedence over env/CLI)"
                );
                v.clone()
            }
            None => String::new(),
        }
    }
    let gitlab_token = env_fallback(&file.gitlab.token, &overrides.gitlab_token, "token");
    let gitlab_webhook_secret = env_fallback(
        &file.gitlab.webhook_secret,
        &overrides.gitlab_webhook_secret,
        "webhook secret",
    );
    let gitlab_signing_secret = env_fallback(
        &file.gitlab.webhook_signing_secret,
        &overrides.gitlab_webhook_signing_secret,
        "webhook signing secret",
    );
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
                internal_base_url: p.internal_base_url.clone(),
                token: p.token.clone(),
                webhook_secret: p.webhook_secret.clone(),
                webhook_signing_secret: p.webhook_signing_secret.clone(),
                allowed_projects: p.allowed_projects.clone(),
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
                internal_base_url: String::new(),
                token: "glpat-platform".to_string(),
                webhook_secret: "wh-secret".to_string(),
                webhook_signing_secret: String::new(),
                allowed_projects: Vec::new(),
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

    // ── Secret encryption at rest (config key storage) ──

    #[test]
    fn encrypt_decrypt_round_trip() {
        let key = [7u8; 32];
        let plain = "glpat-super-secret";
        let enc = secrets::encrypt_secret(plain, &key).unwrap();
        assert!(
            enc.starts_with(ENC_PREFIX),
            "encrypted value must carry the enc: prefix: {enc}"
        );
        assert_eq!(secrets::decrypt_secret(&enc, &key).unwrap(), plain);
        // A fresh nonce per call: same plaintext, different ciphertext.
        let enc2 = secrets::encrypt_secret(plain, &key).unwrap();
        assert_ne!(enc, enc2);
    }

    #[test]
    fn decrypt_passes_through_legacy_plaintext() {
        let key = [9u8; 32];
        assert_eq!(secrets::decrypt_secret("glpat-plain", &key).unwrap(), "glpat-plain");
        assert_eq!(secrets::decrypt_secret("", &key).unwrap(), "");
        // A value that merely CONTAINS enc: mid-string is plaintext.
        assert_eq!(secrets::decrypt_secret("abc enc: def", &key).unwrap(), "abc enc: def");
    }

    #[test]
    fn decrypt_corrupt_encrypted_value_is_an_error() {
        let key = [9u8; 32];
        let enc = secrets::encrypt_secret("secret", &key).unwrap();
        // Wrong key => AEAD authentication failure.
        assert!(secrets::decrypt_secret(&enc, &[8u8; 32]).is_err());
        // Truncated payload and invalid base64 are both corruption.
        assert!(secrets::decrypt_secret("enc:AAAA", &key).is_err());
        assert!(secrets::decrypt_secret("enc:!!!", &key).is_err());
    }

    /// First save auto-creates `secrets.key` next to ui-state.toml: 0600 on
    /// Unix, exactly 32 bytes, written atomically (no leftover temp file).
    #[test]
    fn save_creates_local_key_with_0600_and_32_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(UI_STATE_FILE_NAME);
        save_ui_state(&path, &sample_state_file()).unwrap();

        let key_path = secrets::key_path_for(&path);
        assert!(key_path.exists(), "secrets.key must be created on first save");
        let meta = std::fs::metadata(&key_path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = meta.permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "secrets.key must be 0600, got {mode:o}");
        }
        let bytes = std::fs::read(&key_path).unwrap();
        assert_eq!(bytes.len(), 32, "key must be exactly 32 bytes");
        assert!(!key_path.with_extension("key.tmp").exists());
    }

    /// Save encrypts git credentials at rest (file carries only `enc:`
    /// values), and load decrypts them back to plaintext — memory and the
    /// replay path stay plaintext.
    #[test]
    fn save_load_round_trip_encrypts_secrets_at_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        save_ui_state(&path, &sample_state_file()).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains(ENC_PREFIX), "at-rest secrets must be encrypted: {raw}");
        // base64 uses no '-', so none of these plaintexts can be a substring
        // of any enc: value.
        assert!(
            !raw.contains("glpat-platform"),
            "plaintext token must not be at rest: {raw}"
        );
        assert!(
            !raw.contains("wh-secret"),
            "plaintext webhook secret must not be at rest: {raw}"
        );
        assert!(
            !raw.contains("glpat-legacy"),
            "plaintext legacy token must not be at rest: {raw}"
        );
        assert!(
            !raw.contains("legacy-wh"),
            "plaintext legacy webhook secret must not be at rest: {raw}"
        );
        // Non-secret fields stay plaintext on disk.
        assert!(raw.contains("http://gitlab.internal:8929"));

        let loaded = load_ui_state(&path).unwrap().expect("file must load");
        assert_eq!(loaded.git_platforms[0].token, "glpat-platform");
        assert_eq!(loaded.git_platforms[0].webhook_secret, "wh-secret");
        assert_eq!(loaded.git_platforms[0].base_url, "http://gitlab.internal:8929");
        assert_eq!(loaded.gitlab.token, "glpat-legacy");
        assert_eq!(loaded.gitlab.webhook_secret, "legacy-wh");
        // Empty secrets round-trip as empty (never encrypted, never enc:).
        assert!(loaded.git_platforms[0].webhook_signing_secret.is_empty());
        assert!(loaded.gitlab.webhook_signing_secret.is_empty());
    }

    /// Legacy all-plaintext file: loads fine with NO key file present — the
    /// transparent migration path. The next save encrypts it.
    #[test]
    fn load_legacy_plaintext_file_without_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        let plain = r#"
[[git_platforms]]
name = "old"
type = "gitlab"
base_url = "https://old.internal"
token = "glpat-old-plain"
webhook_secret = "old-wh"

[gitlab]
token = "glpat-legacy-plain"
webhook_secret = "legacy-wh-plain"
"#;
        std::fs::write(&path, plain).unwrap();
        assert!(
            !secrets::key_path_for(&path).exists(),
            "precondition: no key file anywhere in the temp dir"
        );
        let loaded = load_ui_state(&path).unwrap().expect("plaintext file must load");
        assert_eq!(loaded.git_platforms[0].token, "glpat-old-plain");
        assert_eq!(loaded.git_platforms[0].webhook_secret, "old-wh");
        assert_eq!(loaded.gitlab.token, "glpat-legacy-plain");
        assert_eq!(loaded.gitlab.webhook_secret, "legacy-wh-plain");
    }

    /// An `enc:` value with no `secrets.key` must fail loudly with a message
    /// pointing at the missing key and the recovery path — never a panic,
    /// never a silent swallow.
    #[test]
    fn load_encrypted_file_without_key_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        let enc = secrets::encrypt_secret("glpat-then-key-lost", &[0x42u8; 32]).unwrap();
        let toml = format!(
            "[[git_platforms]]\nname = \"p\"\ntype = \"gitlab\"\nbase_url = \"https://p.internal\"\ntoken = \"{enc}\"\n"
        );
        std::fs::write(&path, toml).unwrap();
        assert!(
            !secrets::key_path_for(&path).exists(),
            "precondition: no key file (key was never written to disk)"
        );

        let err = load_ui_state(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("secrets.key"),
            "error must point at the missing key: {msg}"
        );
        assert!(msg.contains("re-enter"), "error must explain recovery: {msg}");
    }

    /// A corrupt key file (wrong length) is an error, not a silent regenerate.
    #[test]
    fn load_encrypted_file_with_corrupt_key_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        let enc = secrets::encrypt_secret("glpat-then-key-lost", &[0x42u8; 32]).unwrap();
        let toml = format!(
            "[[git_platforms]]\nname = \"p\"\ntype = \"gitlab\"\nbase_url = \"https://p.internal\"\ntoken = \"{enc}\"\n"
        );
        std::fs::write(&path, toml).unwrap();
        let key_path = secrets::key_path_for(&path);
        std::fs::write(&key_path, [0u8; 16]).unwrap();

        let err = load_ui_state(&path).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("corrupted"), "corrupt key must be reported: {msg}");
        assert!(msg.contains("32 bytes"), "message must name the expected length: {msg}");
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
    async fn persisted_file_wins_over_env_overrides() {
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
            gitlab_webhook_secret: Some("wh-env".to_string()),
            llm_from_env: true,
            ..Default::default()
        };
        load_and_apply_ui_state(&state, &path, &overrides).unwrap();

        // The persisted file is the authoritative source for gitlab: its
        // values win even when env/CLI supplies different ones.
        let rt = crate::server::gitlab::gitlab_runtime().read().unwrap().clone();
        assert_eq!(rt.token, "glpat-legacy", "file token must beat the env override");
        assert_eq!(
            rt.webhook_secret, "legacy-wh",
            "file webhook secret must beat the env override"
        );
        // LLM is unchanged by the inversion: env still wins wholesale.
        let llm = state.llm_configs.read().unwrap().clone();
        assert_eq!(llm.len(), 1);
        assert_eq!(llm[0].api_key, "sk-env", "persisted sk-live must not override env");
        assert_eq!(llm[0].model, "gpt-env");
        // Platforms have no env source: the persisted entry applies.
        assert_eq!(state.git_platforms.read().unwrap()[0].name, "testbed");
    }

    #[tokio::test]
    async fn env_fallback_used_only_when_file_value_empty() {
        let _lock = RUNTIME_TEST_LOCK.lock().await;
        let _guard = RuntimeGuard::new();
        // Startup runtime seeded from env, as `init_gitlab_runtime` does.
        *crate::server::gitlab::gitlab_runtime().write().unwrap() = crate::server::gitlab::GitLabRuntimeConfig {
            webhook_secret: String::new(),
            signing_secret: None,
            signing_key: None,
            token: String::new(),
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        // Construct the UiStateFile directly with PLAINTEXT values (no
        // save_ui_state, so no at-rest encryption): the gitlab section is
        // entirely empty, so the replay must fall back to env/CLI.
        let file = UiStateFile {
            ui: None,
            llm: vec![],
            git_platforms: vec![],
            gitlab: PersistedGitlabConfig::default(),
        };
        std::fs::write(&path, toml::to_string(&file).unwrap()).unwrap();

        let state = Arc::new(fresh_state(vec![]));
        let overrides = UiStateEnvOverrides {
            gitlab_token: Some("glpat-env".to_string()),
            gitlab_webhook_secret: Some("wh-env".to_string()),
            ..Default::default()
        };
        // Must apply without panicking and land the env fallback values.
        load_and_apply_ui_state(&state, &path, &overrides).unwrap();
        let rt = crate::server::gitlab::gitlab_runtime().read().unwrap().clone();
        assert_eq!(rt.token, "glpat-env", "env fallback fills an empty file token");
        assert_eq!(
            rt.webhook_secret, "wh-env",
            "env fallback fills an empty file webhook secret"
        );
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

    // ── Non-openai env primary: env secret never persisted, masked projection kept ──

    /// The env-seeded non-openai primary entry (token-plan xiaomi-mimo): the
    /// legacy scalar fields describe this primary (whatever its name), so the
    /// masked round trip and any relabeling reconstruction revolve around it.
    fn env_xiaomi_entry() -> LLMConfig {
        LLMConfig {
            provider: "xiaomi-mimo".to_string(),
            model: "mimo-v2.5-pro".to_string(),
            api_key: "tp-REALKEY".to_string(),
            api_base: "https://token-plan-cn.xiaomimimo.com/v1".to_string(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
        }
    }

    /// State as the server looks at PUT time when booted with the
    /// xiaomi-mimo entry from env: the entry is effective in the runtime and
    /// tracked as env-derived.
    fn state_with_env_xiaomi(path: &Path) -> AppState {
        let mut state = fresh_state(vec![env_xiaomi_entry()]);
        state.ui_state_path = Some(path.to_path_buf());
        state.ui_state_env = Some(UiStateEnvOverrides {
            llm_from_env: true,
            llm_entries: vec![env_xiaomi_entry()],
            ..Default::default()
        });
        state
    }

    /// A sparse PUT touching only `gitPlatforms` — no llm section at all.
    fn sparse_git_platform_put() -> serde_json::Value {
        serde_json::json!({
            "gitPlatforms": [{
                "name": "t",
                "type": "gitlab",
                "baseUrl": "http://g.internal",
                "token": "glpat-x"
            }]
        })
    }

    /// A sparse PUT over a stored projection seeded from a non-openai env
    /// primary must persist NOTHING env-derived: the seeded key stays masked
    /// in the stored projection, so the merge cannot smuggle the live secret
    /// into the save pipeline (where the legacy scalar path would rebuild it
    /// with a hardcoded `provider: "openai"` label).
    #[tokio::test]
    async fn sparse_put_never_persists_env_non_openai_entry() {
        let _lock = RUNTIME_TEST_LOCK.lock().await;
        let _guard = RuntimeGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        let state = Arc::new(state_with_env_xiaomi(&path));

        let resp = crate::server::api::config::put_config(
            axum::extract::State(state.clone()),
            axum::Json(sparse_git_platform_put()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("tp-REALKEY"), "env secret must never be persisted: {raw}");
        assert!(
            !raw.contains("provider = \"openai\""),
            "no relabeled openai entry may reach disk: {raw}"
        );
        let file = load_ui_state(&path).unwrap().expect("file must load");
        assert!(
            file.llm.is_empty(),
            "the env-derived entry must not be persisted: {:?}",
            file.llm
        );
    }

    /// Sparse and full PUTs of the same stored (masked) projection resolve to
    /// the same applied LLM set, so both persist the same llm section — here
    /// the empty one, the only entry being env-derived.
    #[tokio::test]
    async fn sparse_and_full_put_persist_identical_llm() {
        let _lock = RUNTIME_TEST_LOCK.lock().await;
        let _guard = RuntimeGuard::new();
        let dir = tempfile::tempdir().unwrap();

        let path_sparse = dir.path().join("sparse.toml");
        let state_sparse = Arc::new(state_with_env_xiaomi(&path_sparse));
        let resp = crate::server::api::config::put_config(
            axum::extract::State(state_sparse.clone()),
            axum::Json(sparse_git_platform_put()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let path_full = dir.path().join("full.toml");
        let state_full = Arc::new(state_with_env_xiaomi(&path_full));
        let resp = crate::server::api::config::put_config(
            axum::extract::State(state_full.clone()),
            axum::Json(serde_json::json!({
                "llm": {
                    "openaiApiKey": API_KEY_MASK,
                    "apiBaseUrl": "https://token-plan-cn.xiaomimimo.com/v1",
                    "defaultModel": "mimo-v2.5-pro",
                    "providers": [{
                        "provider": "xiaomi-mimo",
                        "apiKey": API_KEY_MASK,
                        "apiBaseUrl": "https://token-plan-cn.xiaomimimo.com/v1",
                        "defaultModel": "mimo-v2.5-pro"
                    }]
                }
            })),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let sparse = load_ui_state(&path_sparse).unwrap().unwrap();
        let full = load_ui_state(&path_full).unwrap().unwrap();
        assert!(
            sparse.llm.is_empty(),
            "sparse PUT must persist an empty llm section: {:?}",
            sparse.llm
        );
        assert!(
            full.llm.is_empty(),
            "full PUT must persist an empty llm section: {:?}",
            full.llm
        );
        assert_eq!(
            serde_json::to_value(&sparse.llm).unwrap(),
            serde_json::to_value(&full.llm).unwrap(),
            "sparse and full PUT must persist identical llm sections"
        );
    }

    /// A reconstruction path that relabels the env entry's provider to
    /// "openai" but keeps its secret + base must still be recognized as
    /// env-derived: identity is the credential, not the label.
    #[test]
    fn from_applied_filters_relabeled_env_entry() {
        let mut ui = UiConfig::default();
        ui.llm.openai_api_key = API_KEY_MASK.to_string();
        let applied = AppliedConfig {
            llm: vec![LLMConfig {
                provider: "openai".to_string(),
                model: "mimo-v2.5-pro".to_string(),
                api_key: "tp-REALKEY".to_string(),
                api_base: "https://token-plan-cn.xiaomimimo.com/v1".to_string(),
                max_tokens: 4096,
                temperature: 0.3,
                disable_thinking: None,
            }],
            git_platforms: vec![],
            gitlab_token: String::new(),
            gitlab_webhook_secret: String::new(),
            gitlab_webhook_signing_secret: String::new(),
            ui,
        };
        let env = UiStateEnvOverrides {
            llm_from_env: true,
            llm_entries: vec![env_xiaomi_entry()],
            ..Default::default()
        };

        let file = UiStateFile::from_applied(&applied, Some(&env));
        assert!(
            file.llm.is_empty(),
            "secret+base identity beats the provider label: {:?}",
            file.llm
        );
        assert!(
            file.ui.unwrap().llm.openai_api_key.is_empty(),
            "no mask marker for a key the file does not hold"
        );
    }

    /// The legacy scalar key field echoes the PRIMARY provider's key, so the
    /// mask marker must key off the effective primary: after a sparse PUT
    /// with a non-openai primary the stored projection keeps `***` (a
    /// configured key must not read as unset), and the authoritative store
    /// keeps exactly the env-seeded entry.
    #[tokio::test]
    async fn sparse_put_keeps_masked_projection_for_non_openai_primary() {
        let _lock = RUNTIME_TEST_LOCK.lock().await;
        let _guard = RuntimeGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        let state = Arc::new(state_with_env_xiaomi(&path));

        let resp = crate::server::api::config::put_config(
            axum::extract::State(state.clone()),
            axum::Json(sparse_git_platform_put()),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        assert_eq!(
            state.ui_config.read().unwrap().llm.openai_api_key,
            API_KEY_MASK,
            "a configured non-openai primary must read as masked, not unset"
        );

        let llm = state.app_config.read().unwrap().as_ref().unwrap().llm.clone();
        assert_eq!(llm.len(), 1, "app_config.llm must hold exactly one entry: {llm:?}");
        assert_eq!(llm[0].provider, "xiaomi-mimo");
        assert_eq!(llm[0].api_key, "tp-REALKEY");
        assert_eq!(llm[0].api_base, "https://token-plan-cn.xiaomimimo.com/v1");
    }

    // ── 0.10.0: DB-backed persistence (import / DB replay / escape hatch) ──

    async fn fresh_db() -> SqlxStore {
        let store = SqlxStore::new_in_memory().await.unwrap();
        store.migrate().await.unwrap();
        store
    }

    /// (a) One-shot import: an old ui-state.toml (plaintext LLM key on disk,
    /// encrypted git token) imports into empty config tables; the file is
    /// renamed to .migrated; every secret column at rest is `enc:`; and the
    /// DB replay produces the same effective configuration as the 0.9 file
    /// replay.
    #[tokio::test]
    async fn import_then_db_replay_matches_file_replay() {
        let _lock = RUNTIME_TEST_LOCK.lock().await;
        let _guard = RuntimeGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        // save_ui_state writes the legacy on-disk shape: git secrets enc:,
        // the LLM api_key PLAINTEXT (0.9 threat model, persist.rs:27-29).
        save_ui_state(&path, &sample_state_file()).unwrap();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("sk-live"),
            "precondition: 0.9 stores the LLM key in plaintext"
        );

        let store = fresh_db().await;
        assert!(import_ui_state_into_db(&store, &path).await.unwrap());

        assert!(!path.exists(), "original file must be renamed away");
        assert!(migrated_path(&path).exists(), "backup must be kept");
        assert!(!store.config_tables_empty().await.unwrap());

        // At rest: all four secret columns are enc:-prefixed — the LLM key
        // included (newly inside the encryption boundary).
        let api_key: String = sqlx::query_scalar("SELECT api_key FROM llm_providers")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert!(
            api_key.starts_with("enc:"),
            "api_key must be encrypted at rest (enc:-prefixed)"
        );
        assert!(!api_key.contains("sk-live"));
        let token: String = sqlx::query_scalar("SELECT token FROM git_platforms")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert!(token.starts_with("enc:"), "platform token must stay encrypted");
        let gitlab_raw: String = sqlx::query_scalar("SELECT value FROM app_settings WHERE key = 'gitlab'")
            .fetch_one(store.pool())
            .await
            .unwrap();
        let gitlab_json: serde_json::Value = serde_json::from_str(&gitlab_raw).unwrap();
        assert!(gitlab_json["token"].as_str().unwrap().starts_with("enc:"));
        assert!(gitlab_json["webhook_secret"].as_str().unwrap().starts_with("enc:"));
        assert_eq!(gitlab_json["webhook_signing_secret"], "");

        // Replay equivalence: DB replay vs 0.9 file replay (from the backup)
        // must land the same effective configuration.
        let state_db = Arc::new(fresh_state(vec![]));
        assert!(
            load_and_apply_ui_state_from_db(&state_db, &store, &UiStateEnvOverrides::default())
                .await
                .unwrap()
        );
        let state_file = Arc::new(fresh_state(vec![]));
        assert!(load_and_apply_ui_state(&state_file, &migrated_path(&path), &UiStateEnvOverrides::default()).unwrap());
        let db_llm = state_db.llm_configs.read().unwrap().clone();
        let file_llm = state_file.llm_configs.read().unwrap().clone();
        assert_eq!(db_llm.len(), 1);
        assert_eq!(db_llm[0].api_key, "sk-live");
        assert_eq!(db_llm[0].api_key, file_llm[0].api_key);
        assert_eq!(
            state_db.git_platforms.read().unwrap().clone(),
            state_file.git_platforms.read().unwrap().clone()
        );
        assert_eq!(
            serde_json::to_value(&*state_db.ui_config.read().unwrap()).unwrap(),
            serde_json::to_value(&*state_file.ui_config.read().unwrap()).unwrap(),
            "GET /config projection must be identical for DB and file replay"
        );
    }

    /// (b) env precedence matrix against the DB source: env-seeded LLM list
    /// wins wholesale (DB llm section NOT replayed); legacy gitlab env/CLI
    /// values are fallback-only (used only when the DB value is empty).
    #[tokio::test]
    async fn db_replay_env_precedence_matrix() {
        let _lock = RUNTIME_TEST_LOCK.lock().await;
        let _guard = RuntimeGuard::new();
        let env_llm = LLMConfig {
            provider: "openai".to_string(),
            model: "gpt-env".to_string(),
            api_key: "sk-env".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            disable_thinking: None,
        };

        // Row 1: llm_from_env — DB llm entries must not touch the runtime.
        let store = fresh_db().await;
        store
            .replace_llm_providers(&[LLMConfig {
                provider: "openai".to_string(),
                model: "gpt-db".to_string(),
                api_key: "sk-db".to_string(),
                api_base: String::new(),
                max_tokens: 4096,
                temperature: 0.7,
                disable_thinking: None,
            }])
            .await
            .unwrap();
        let state = Arc::new(fresh_state(vec![env_llm.clone()]));
        let overrides = UiStateEnvOverrides {
            llm_from_env: true,
            llm_entries: vec![env_llm.clone()],
            ..Default::default()
        };
        assert!(load_and_apply_ui_state_from_db(&state, &store, &overrides)
            .await
            .unwrap());
        let llm = state.llm_configs.read().unwrap().clone();
        assert_eq!(llm.len(), 1);
        assert_eq!(llm[0].api_key, "sk-env", "env provider list wins wholesale over the DB");
        assert_eq!(llm[0].model, "gpt-env");

        // Row 2: DB gitlab empty → env/CLI fills in (fallback-only).
        *crate::server::gitlab::gitlab_runtime().write().unwrap() = crate::server::gitlab::GitLabRuntimeConfig {
            webhook_secret: String::new(),
            signing_secret: None,
            signing_key: None,
            token: String::new(),
        };
        let state2 = Arc::new(fresh_state(vec![]));
        let overrides2 = UiStateEnvOverrides {
            gitlab_token: Some("glpat-env".to_string()),
            gitlab_webhook_secret: Some("wh-env".to_string()),
            ..Default::default()
        };
        assert!(load_and_apply_ui_state_from_db(&state2, &store, &overrides2)
            .await
            .unwrap());
        let rt = crate::server::gitlab::gitlab_runtime().read().unwrap().clone();
        assert_eq!(rt.token, "glpat-env", "env fallback fills an empty DB token");
        assert_eq!(rt.webhook_secret, "wh-env");

        // Row 3: DB gitlab set → DB is authoritative, env is ignored.
        store
            .save_legacy_gitlab(&PersistedGitlabConfig {
                token: "glpat-db".to_string(),
                webhook_secret: "wh-db".to_string(),
                webhook_signing_secret: String::new(),
            })
            .await
            .unwrap();
        *crate::server::gitlab::gitlab_runtime().write().unwrap() = crate::server::gitlab::GitLabRuntimeConfig {
            webhook_secret: String::new(),
            signing_secret: None,
            signing_key: None,
            token: String::new(),
        };
        let state3 = Arc::new(fresh_state(vec![]));
        assert!(load_and_apply_ui_state_from_db(&state3, &store, &overrides2)
            .await
            .unwrap());
        let rt = crate::server::gitlab::gitlab_runtime().read().unwrap().clone();
        assert_eq!(rt.token, "glpat-db", "DB token must beat the env override");
        assert_eq!(rt.webhook_secret, "wh-db");
    }

    /// (c) A mid-import failure rolls back the whole transaction: no partial
    /// rows, the file is NOT renamed, and the file replay fallback still
    /// works. Injected fault: two git platforms with the same `name` violate
    /// the UNIQUE constraint on the second insert.
    #[tokio::test]
    async fn failed_import_rolls_back_and_keeps_file() {
        let _lock = RUNTIME_TEST_LOCK.lock().await;
        let _guard = RuntimeGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        let mut file = sample_state_file();
        file.git_platforms.push(GitPlatformConfig {
            name: "testbed".to_string(), // duplicate of the first entry
            base_url: "https://dup.example.com".to_string(),
            ..Default::default()
        });
        save_ui_state(&path, &file).unwrap();

        let store = fresh_db().await;
        let err = import_ui_state_into_db(&store, &path).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("testbed"),
            "error must name the failing row: {err:#}"
        );
        assert!(path.exists(), "failed import must NOT rename the file");
        assert!(!migrated_path(&path).exists());
        assert!(
            store.config_tables_empty().await.unwrap(),
            "the transaction must roll back completely (no partial import)"
        );

        // Fallback: the file replay path still applies the (valid parts of
        // the) configuration — same as a corrupt-DB 0.9 startup.
        let state = Arc::new(fresh_state(vec![]));
        assert!(load_and_apply_ui_state(&state, &path, &UiStateEnvOverrides::default()).unwrap());
        assert_eq!(state.llm_configs.read().unwrap()[0].api_key, "sk-live");
    }

    /// PUT /config with a DB attached persists to the DB (not to the file)
    /// and stores the resolved live key encrypted.
    #[tokio::test]
    async fn put_config_persists_to_db_instead_of_file() {
        let _lock = RUNTIME_TEST_LOCK.lock().await;
        let _guard = RuntimeGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(UI_STATE_FILE_NAME);
        let store = fresh_db().await;

        let mut state = fresh_state(vec![]);
        state.ui_state_path = Some(path.clone());
        state.db = Some(Arc::new(store.clone()));
        let state = Arc::new(state);
        let resp = crate::server::api::config::put_config(
            axum::extract::State(state.clone()),
            axum::Json(serde_json::json!({
                "llm": {
                    "openaiApiKey": "sk-live-db",
                    "apiBaseUrl": "https://api.openai.com/v1",
                    "defaultModel": "gpt-4o"
                },
                "gitlab": { "apiToken": "glpat-db", "webhookSecret": "wh-db" }
            })),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        assert!(!path.exists(), "no ui-state.toml may be written when the DB is active");
        let llm = store.load_llm_providers().await.unwrap();
        assert_eq!(llm.len(), 1);
        assert_eq!(
            llm[0].api_key, "sk-live-db",
            "masked/resolved live key must land in the DB"
        );
        let at_rest: String = sqlx::query_scalar("SELECT api_key FROM llm_providers")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert!(at_rest.starts_with("enc:"));
        let gitlab = store.load_legacy_gitlab().await.unwrap();
        assert_eq!(gitlab.token, "glpat-db");
        assert_eq!(gitlab.webhook_secret, "wh-db");

        // And a fresh state replays it back from the DB.
        let state2 = Arc::new(fresh_state(vec![]));
        assert!(
            load_and_apply_ui_state_from_db(&state2, &store, &UiStateEnvOverrides::default())
                .await
                .unwrap()
        );
        assert_eq!(state2.llm_configs.read().unwrap()[0].api_key, "sk-live-db");
    }

    /// (d) The escape hatch: REVIEW_DISABLE_DB parsing. Behavioural 0.9
    /// equivalence is structural — `db = None` routes everything through the
    /// file path, which the tests above (and every pre-existing persist test)
    /// exercise.
    #[test]
    fn db_disabled_flag_parsing() {
        assert!(db_disabled_flag(Some("1")));
        assert!(db_disabled_flag(Some("true")));
        assert!(db_disabled_flag(Some(" TRUE ")));
        assert!(db_disabled_flag(Some("yes")));
        assert!(!db_disabled_flag(None));
        assert!(!db_disabled_flag(Some("")));
        assert!(!db_disabled_flag(Some("0")));
        assert!(!db_disabled_flag(Some("no")));
        assert!(!db_disabled_flag(Some("random")));
    }
}
