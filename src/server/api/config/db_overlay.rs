//! Database-over-file configuration overlay — the `AppState`-free core.
//!
//! Resolution order for any `reng` entry point that resolves a configuration
//! file — `serve` applies it at bootstrap, and the CLI paths use the same
//! call:
//!
//! ```text
//! built-in defaults → env → ~/.config/review-engine/.code-audit-config.toml
//!                    → ./.code-audit-config.toml     ← `resolver::resolve_config`
//!                    → review.db (config dir)        ← [`apply_db_overrides`]
//! ```
//!
//! The database is the HIGHEST-priority layer and it is applied per key: a
//! surface the DB carries (a row, an `app_settings` entry) overrides whatever
//! the TOML chain resolved, and a key the DB does not carry keeps its TOML
//! value. Policy values that live only in the config file — the audit presets
//! in `[report]` / `[scoring]` / `[review_experts]` / the language profiles —
//! therefore survive untouched: nothing in the DB can erase them, only
//! override the entries it actually stores.
//!
//! [`apply_db_overrides`] is deliberately independent of [`crate::server::AppState`]:
//! it takes the already-resolved [`AppConfig`], the store, and the env/CLI
//! overrides, and reports what it applied. That is what lets the CLI paths
//! (which never build an `AppState`) honour the same DB-over-file rule as
//! `serve` — the `serve` bootstrap calls this function too, so both paths
//! share one implementation.
//!
//! ## What maps where
//!
//! | DB surface | lands on |
//! | --- | --- |
//! | `app_settings` `ui` → `advanced.maxConcurrentReviews` | `AppConfig::max_concurrent_llm_calls` **and** `AppConfig::max_team_size` |
//! | `app_settings` `ui` → `aggregated` (RENG-95 tri-state) | `AppConfig::report.aggregated` |
//! | `app_settings` `ui` → `rules` / the remaining `advanced` fields | returned in [`AppliedDbOverrides::ui`] — the UI projection has no `AppConfig` counterpart |
//! | `llm_providers` rows | `AppConfig::llm` — replaced wholesale, in the server's chain order (primary first, disabled entries out) |
//! | `git_platforms` rows | returned in [`AppliedDbOverrides::git_platforms`] — `AppConfig` carries no such field |
//! | `app_settings` `gitlab` | returned in [`AppliedDbOverrides::gitlab`] |
//! | `app_settings` `experts` | `AppConfig::review_experts` (patched entries) |
//!
//! ## Secrets
//!
//! Every value is read through the [`ConfigStore`] boundary, which is where the
//! `enc:` at-rest form is decrypted with the per-config-dir `secrets.key`
//! (`rows.rs`, `crate::config::secrets`). The overlay therefore inherits the
//! SAME key resolution `serve` uses — the store is built over the same config
//! dir (`SqlxStore::connect_default(config_dir)`, `secrets.key` next to
//! `review.db`) — and never handles ciphertext itself.
//!
//! ## Relationship to the WebUI replay in `serve`
//!
//! `serve` calls this function in its bootstrap and then keeps its existing
//! WebUI replay ([`super::persist::load_and_apply_ui_state_from_db`] plus the
//! expert-override replay) — the replay owns surfaces this function cannot
//! express (the `ui_config` projection, the GitLab runtime, the masked
//! shapes), so it is not replaced. The two paths agree, by construction, on:
//!
//! - the fields both write (`AppConfig::llm`, `report.aggregated`, the two
//!   concurrency caps) — the replay runs LAST and recomputes them from the
//!   same rows, so its value wins and this function only decides what the
//!   config holds before the replay runs;
//! - the expert overrides — the replay re-applies the same map (see the
//!   `serve` call site, which pins `AppState::expert_base` to the
//!   FILE-resolved `[review_experts]` so RENG-93's "clearing an override
//!   restores the file value" still holds).
//!
//! Two narrow divergences are known and accepted. Both need a database whose
//! stored key is not a live one — empty or the `***` mask. The UI save path
//! only ever persists keys it resolved to a real value, but nothing else
//! guarantees that: the one-shot `ui-state.toml` import carries whatever the
//! file held (`load_ui_state` does not validate keys) and a hand-edited row
//! can hold anything.
//!
//! 1. Such a row is dropped here, while the replay's masked-keep resolution
//!    ([`super::is_blank_or_masked`]) instead tries to resolve the key against
//!    the config it keeps in memory — which after this function ran is this
//!    chain, not the TOML/env one it used to be. A row whose
//!    `(provider, api_base, model)` triple also exists in the TOML file could
//!    therefore inherit that file's key, and is dropped now.
//! 2. That keep-resolution's SOURCE is consequently the DB chain rather than
//!    the TOML chain.
//!
//! ## Env/CLI values
//!
//! `env` carries what the CLI flags / environment supplied at startup, with the
//! precedence the persistence layer documents ([`UiStateEnvOverrides`]): an
//! env-seeded LLM chain wins WHOLESALE (the `llm_providers` table is then not
//! applied at all), and the legacy GitLab credentials are FALLBACK-ONLY — used
//! where the DB row has no value. No deprecation warning is logged here: the
//! surface that actually consumes the fallback owns that notice (`serve`'s
//! `replay_payload`), so a deployment setting both a DB value and an env value
//! is not warned twice for one credential.
//!
//! ## Errors
//!
//! A store-level failure is returned to the caller, which decides whether to
//! fall back (the DB carries no configuration) or to report it. The error path
//! is atomic with respect to `config`: every fallible read happens before the
//! first mutation, so an `Err` never leaves a partially overlaid config.
//!
//! The `experts` row is the exception: its read failure degrades to "no
//! overrides" with a WARN, because a hand-edited row must not be able to stop
//! the other surfaces from applying (the same contract
//! [`super::persist::load_and_apply_expert_overrides`] documents).

use anyhow::Context;

use crate::models::{AppConfig, GitPlatformConfig, LLMConfig};
use crate::store::traits::ConfigStore;
use crate::store::SqlxStore;

use super::persist::{load_expert_overrides, PersistedGitlabConfig, UiStateEnvOverrides, UI_SETTING_KEY};
use super::types::UiConfig;

/// What the database contributed when [`apply_db_overrides`] ran.
///
/// Each field is the DB's own value — empty / `None` when the DB carries
/// nothing for that surface, in which case the caller's config keeps whatever
/// the TOML resolution produced. The fields that have an `AppConfig`
/// counterpart are ALSO already applied to the config handed in; the rest are
/// returned because they have no place on `AppConfig` (see the module table).
///
/// **Empty from a failed call**: an `Err` from [`apply_db_overrides`] returns
/// nothing at all and leaves the caller's config untouched (every fallible
/// store read happens before the first mutation), so the documented fallback —
/// "treat the database as carrying no configuration" — is always safe.
#[derive(Clone, Default)]
pub struct AppliedDbOverrides {
    /// `app_settings` row `ui` — the persisted UI projection (rules,
    /// advanced, aggregated, and the masked llm/gitPlatform sub-sections).
    /// `None` when the row does not exist. The `advanced` / `aggregated`
    /// values are applied to the config; `rules` have no `AppConfig`
    /// counterpart and are only reported.
    pub ui: Option<UiConfig>,
    /// The provider chain the DB contributed, EXACTLY as it landed on
    /// `AppConfig::llm`: the server's authoritative chain order — the persisted
    /// `ui.llm.primaryProvider` first, then the remaining entries in their
    /// stored order, with DISABLED entries left out
    /// ([`crate::llm::ordered_llm_configs`], RENG-55 / RENG-75).
    ///
    /// A caller that runs reviews on this chain gets the head provider the Web
    /// UI shows as primary without re-ordering anything itself. The rule is the
    /// one the server's own [`crate::server::AppState::ordered_llm_configs`]
    /// applies to the same rows. Empty when the DB carries no provider with a
    /// usable key (a stored key that is blank or the `***` mask is not one —
    /// see the module divergences), or when env supplied the chain.
    pub llm: Vec<LLMConfig>,
    /// The git platform set the DB carries (live secrets, already decrypted).
    /// Never empty when rows exist; `AppConfig` has no field to land it on, so
    /// the caller owns it (`AppState::git_platforms` in `serve`).
    pub git_platforms: Vec<GitPlatformConfig>,
    /// The legacy GitLab credentials, DB value first with the env/CLI value as
    /// the fallback for a field the DB leaves empty.
    pub gitlab: PersistedGitlabConfig,
    /// How many `[review_experts]` entries the persisted override map patched.
    pub experts_patched: usize,
    /// Whether the DATABASE itself carried a legacy GitLab credential, BEFORE
    /// the env/CLI fallback above filled [`Self::gitlab`]. Private on purpose:
    /// it feeds [`Self::is_empty`] only, and a caller asking "did the DB carry
    /// configuration" must not be answered by a fallback the DB never stored.
    gitlab_stored: bool,
}

impl AppliedDbOverrides {
    /// True when the database carried no configuration at all — the caller's
    /// config is the plain TOML resolution and nothing needs republishing.
    ///
    /// Deliberately based on what the DB itself holds: the env/CLI fallback
    /// fills [`Self::gitlab`] into a database that stored no credential, and
    /// an override that named an expert the file does not define patched no
    /// entry — neither counts as "the DB carried configuration".
    pub fn is_empty(&self) -> bool {
        self.ui.is_none()
            && self.llm.is_empty()
            && self.git_platforms.is_empty()
            && !self.gitlab_stored
            && self.experts_patched == 0
    }
}

/// Redacted: the result carries LIVE secrets (git platform tokens, LLM API
/// keys, GitLab credentials), so a derived `Debug` would print them wherever a
/// caller `{:?}`-logs the outcome. What a diagnostic needs — which surfaces the
/// DB carried, which providers/cards they name, and whether a credential is
/// set — is printed instead of the values.
impl std::fmt::Debug for AppliedDbOverrides {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let llm: Vec<String> = self.llm.iter().map(|c| format!("{}:{}", c.provider, c.model)).collect();
        let platforms: Vec<&str> = self.git_platforms.iter().map(|p| p.name.as_str()).collect();
        f.debug_struct("AppliedDbOverrides")
            .field("ui", &self.ui.as_ref().map(|ui| ui.rules.min_score))
            .field("llm", &llm)
            .field("git_platforms", &platforms)
            .field(
                "gitlab",
                &format_args!(
                    "token={} webhook_secret={} signing_secret={}",
                    secret_state(&self.gitlab.token),
                    secret_state(&self.gitlab.webhook_secret),
                    secret_state(&self.gitlab.webhook_signing_secret),
                ),
            )
            .field("experts_patched", &self.experts_patched)
            .field("gitlab_stored", &self.gitlab_stored)
            .finish()
    }
}

/// `set` / `unset` for a live credential — never the value itself.
fn secret_state(secret: &str) -> &'static str {
    if secret.is_empty() {
        "unset"
    } else {
        "set"
    }
}

/// Overlay the configuration the database carries onto `config`, key by key.
///
/// `config` is mutated in place with the surfaces that have an `AppConfig`
/// counterpart (LLM chain, expert overrides, concurrency caps, the aggregation
/// flag) and the rest is returned in [`AppliedDbOverrides`]. A surface the DB
/// does not carry is left exactly as the caller resolved it.
///
/// `env` is the CLI/env tracking of the caller ([`UiStateEnvOverrides`]); pass
/// [`UiStateEnvOverrides::default`] when the caller has none.
///
/// Returns an error only for a store-level failure, and an error leaves
/// `config` UNTOUCHED: every fallible read happens before the first mutation,
/// so a caller may treat `Err` exactly like "the database carries no
/// configuration" and keep the config it resolved. The `experts` row is the
/// one non-fatal read (see the module docs): it degrades to "no overrides"
/// with a WARN and never fails the call.
pub async fn apply_db_overrides(
    config: &mut AppConfig,
    store: &SqlxStore,
    env: &UiStateEnvOverrides,
) -> anyhow::Result<AppliedDbOverrides> {
    // ── Read phase: every fallible store read, before the first mutation, so
    //    an `Err` cannot leave a partially overlaid config behind. ───────
    let ui: Option<UiConfig> = store
        .load_setting(UI_SETTING_KEY)
        .await?
        .map(|value| serde_json::from_value(value).context("app_settings row 'ui' is not a valid UiConfig"))
        .transpose()?;
    // `config.toml < ui-state.toml / review.db < env`: an env-seeded chain
    // wins wholesale, so the table is not read at all then.
    let stored_llm = if env.llm_from_env {
        Vec::new()
    } else {
        store.load_llm_providers().await?
    };
    let stored_platforms = store.load_git_platforms().await?;
    let stored_gitlab = store.load_legacy_gitlab().await?;
    let expert_overrides = match load_expert_overrides(store).await {
        Ok(overrides) => overrides,
        Err(e) => {
            // Field named `reason`, not `error`: the log collector infers a
            // plain-text line's level by substring (`infer_level_from_line`),
            // and an `error=…` field would file this WARN as an ERROR.
            tracing::warn!(
                reason = %format!("{e:#}"),
                "ignoring the persisted expert overrides: the settings row could not be read; \
                 the config file's [review_experts] values stand"
            );
            crate::config::ExpertOverrides::default()
        }
    };

    // ── Apply phase: infallible from here on. ───────────────────────────
    let mut applied = AppliedDbOverrides {
        ui,
        git_platforms: stored_platforms,
        gitlab_stored: !stored_gitlab.is_empty(),
        ..Default::default()
    };

    if let Some(ui) = &applied.ui {
        // `advanced.maxConcurrentReviews` is the UI's name for the two caps the
        // backend enforces; they are set together, exactly as the `PUT /config`
        // pipeline sets them (`apply_ui_config`).
        let concurrency = ui.advanced.max_concurrent_reviews as usize;
        config.max_concurrent_llm_calls = Some(concurrency);
        config.max_team_size = Some(concurrency);
        // RENG-95: tri-state — only a row that actually decided the flag
        // overrides the config file. `None` (a row written before the toggle
        // existed) leaves the file's value in force.
        if let Some(aggregated) = ui.aggregated {
            config.report.aggregated = aggregated;
        }
    }

    // ── llm_providers ───────────────────────────────────────────────────
    if env.llm_from_env {
        // The env-seeded chain (read phase) wins wholesale.
        tracing::debug!(
            "the LLM_CONFIG/env provider chain wins over the persisted llm_providers table; \
             the database chain is not applied"
        );
    } else {
        // A stored card whose key is blank or masked never reaches the chain —
        // the SAME rule `apply_ui_config` applies to a card the user emptied
        // ([`super::is_blank_or_masked`]), so both DB-apply paths agree on what
        // "a configured provider" is. A masked row would otherwise enter the
        // chain as the literal `***` and hand that to the provider.
        let usable: Vec<LLMConfig> = stored_llm
            .into_iter()
            .filter(|c| !super::is_blank_or_masked(&c.api_key))
            .collect();
        if !usable.is_empty() {
            // RENG-55 / RENG-75: the chain order is the server's, not the
            // stored one — the persisted primary first (when the `ui` row
            // names one), the rest in stored order, disabled entries excluded.
            // A caller that runs reviews straight off this chain therefore
            // runs the head provider the Web UI shows as primary.
            let primary = applied
                .ui
                .as_ref()
                .map(|ui| ui.llm.primary_provider.as_str())
                .unwrap_or_default();
            let chain = crate::llm::ordered_llm_configs(primary, &usable);
            config.llm = chain.clone();
            applied.llm = chain;
        }
    }

    // ── app_settings `gitlab` (legacy credentials) ──────────────────────
    let env_fallback = |stored: String, env: &Option<String>| -> String {
        match env {
            Some(value) if stored.is_empty() => value.clone(),
            _ => stored,
        }
    };
    applied.gitlab = PersistedGitlabConfig {
        token: env_fallback(stored_gitlab.token, &env.gitlab_token),
        webhook_secret: env_fallback(stored_gitlab.webhook_secret, &env.gitlab_webhook_secret),
        webhook_signing_secret: env_fallback(stored_gitlab.webhook_signing_secret, &env.gitlab_webhook_signing_secret),
    };

    // ── app_settings `experts` (WebUI expert overrides) ─────────────────
    if !expert_overrides.is_empty() {
        applied.experts_patched = expert_overrides.apply_to(config);
        if applied.experts_patched < expert_overrides.len() {
            tracing::debug!(
                stored = expert_overrides.len(),
                applied = applied.experts_patched,
                "expert override(s) name an expert the config file does not define; skipped"
            );
        }
    }

    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ExpertOverride;
    use crate::models::ExpertTomlDef;

    /// The TOML-resolved shape the overlay starts from: a provider chain, the
    /// policy values that live ONLY in the config file (an audit-preset-style
    /// `[report]`, the two concurrency caps, a two-expert team), all of which
    /// must survive an overlay by a database that does not carry them.
    fn file_config() -> AppConfig {
        serde_json::from_value(serde_json::json!({
            "llm": [{
                "provider": "openai",
                "model": "gpt-4o",
                "api_key": "sk-from-toml",
                "api_base": "https://api.openai.com/v1",
                "max_tokens": 4096,
                "temperature": 0.7
            }],
            "review_experts": {
                "Security": { "enabled": true, "weight": 50, "role": "security", "prompt": "file prompt" },
                "Quality": { "enabled": true, "weight": 50, "role": "quality" }
            },
            "report": { "aggregated": false, "max_findings_per_expert": 7 },
            "max_concurrent_llm_calls": 2,
            "max_team_size": 2
        }))
        .expect("the fixture must deserialize as AppConfig")
    }

    fn db_llm(provider: &str, model: &str, key: &str) -> LLMConfig {
        LLMConfig {
            provider: provider.to_string(),
            model: model.to_string(),
            api_key: key.to_string(),
            api_base: "https://api.db.example/v1".to_string(),
            max_tokens: 8192,
            temperature: 0.4,
            disable_thinking: Some(true),
            disabled: false,
        }
    }

    fn db_platform(name: &str, token: &str) -> GitPlatformConfig {
        GitPlatformConfig {
            name: name.to_string(),
            platform_type: "gitlab".to_string(),
            base_url: "http://gitlab.internal:8929".to_string(),
            token: token.to_string(),
            webhook_secret: "wh-db".to_string(),
            ..Default::default()
        }
    }

    async fn fresh_db() -> SqlxStore {
        let store = SqlxStore::new_in_memory().await.unwrap();
        store.migrate().await.unwrap();
        store
    }

    /// A database that stored nothing contributes nothing even when the
    /// environment supplies a GitLab credential: the env/CLI fallback fills the
    /// RUNTIME value, it does not make the DB "carry configuration". A caller
    /// (or `serve`'s startup log) that keyed off `is_empty()` must not be told
    /// otherwise.
    #[tokio::test]
    async fn empty_database_with_an_env_gitlab_token_still_reports_nothing_applied() {
        let store = fresh_db().await;
        let env = UiStateEnvOverrides {
            gitlab_token: Some("glpat-env".to_string()),
            ..Default::default()
        };
        let mut config = file_config();

        let applied = apply_db_overrides(&mut config, &store, &env).await.unwrap();

        assert_eq!(
            applied.gitlab.token, "glpat-env",
            "the fallback still fills the runtime value"
        );
        assert!(
            applied.is_empty(),
            "but the DB stored no credential, so it carried no configuration"
        );
    }

    /// A database whose only row is a credential the caller must not count as
    /// "configuration the DB carries" through the fallback path: with a stored
    /// credential `is_empty()` is false.
    #[tokio::test]
    async fn stored_gitlab_credential_is_reported_as_carried() {
        let store = fresh_db().await;
        store
            .save_legacy_gitlab(&PersistedGitlabConfig {
                webhook_secret: "wh-db".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut config = file_config();

        let applied = apply_db_overrides(&mut config, &store, &UiStateEnvOverrides::default())
            .await
            .unwrap();

        assert!(!applied.is_empty());
        assert_eq!(applied.gitlab.webhook_secret, "wh-db");
    }

    /// An `Err` leaves the caller's config exactly as it resolved it — every
    /// fallible read happens before the first mutation, so "treat the failure
    /// like an empty database" is always safe.
    #[tokio::test]
    async fn store_failure_leaves_the_config_untouched() {
        let store = fresh_db().await;
        // Rows the overlay would otherwise apply, plus a broken store: the
        // `git_platforms` read fails AFTER the LLM read succeeded, which is
        // exactly the shape that used to leave a half-overlaid config.
        store
            .replace_llm_providers(&[db_llm("anthropic", "claude-3-opus", "sk-db")])
            .await
            .unwrap();
        sqlx::query("DROP TABLE git_platforms")
            .execute(store.pool())
            .await
            .unwrap();

        let mut config = file_config();
        let err = apply_db_overrides(&mut config, &store, &UiStateEnvOverrides::default())
            .await
            .unwrap_err();

        assert!(
            format!("{err:#}").contains("git_platforms"),
            "the store failure must be surfaced: {err:#}"
        );
        assert_eq!(config.llm.len(), 1, "no partial overlay");
        assert_eq!(config.llm[0].provider, "openai");
        assert_eq!(config.llm[0].api_key, "sk-from-toml");
        assert_eq!(config.max_concurrent_llm_calls, Some(2));
    }

    /// No DB rows at all → the plain TOML result stands, key by key.
    #[tokio::test]
    async fn empty_database_keeps_the_toml_result() {
        let store = fresh_db().await;
        let mut config = file_config();

        let applied = apply_db_overrides(&mut config, &store, &UiStateEnvOverrides::default())
            .await
            .unwrap();

        assert!(applied.is_empty(), "an empty DB must contribute nothing");
        assert_eq!(config.llm.len(), 1);
        assert_eq!(config.llm[0].api_key, "sk-from-toml");
        assert_eq!(config.max_concurrent_llm_calls, Some(2));
        assert_eq!(config.max_team_size, Some(2));
        assert!(!config.report.aggregated);
        assert_eq!(config.review_experts["Security"].weight, 50);
        assert_eq!(config.review_experts["Security"].prompt.as_deref(), Some("file prompt"));
    }

    /// Every surface the database carries overrides the file.
    #[tokio::test]
    async fn database_rows_override_the_toml_values() {
        let store = fresh_db().await;
        store
            .replace_llm_providers(&[db_llm("anthropic", "claude-3-opus", "sk-db")])
            .await
            .unwrap();
        store
            .replace_git_platforms(&[db_platform("testbed", "glpat-db")])
            .await
            .unwrap();
        store
            .save_legacy_gitlab(&PersistedGitlabConfig {
                token: "glpat-legacy-db".to_string(),
                webhook_secret: "wh-legacy-db".to_string(),
                webhook_signing_secret: String::new(),
            })
            .await
            .unwrap();
        let ui: UiConfig = serde_json::from_value(serde_json::json!({
            "rules": { "minScore": 91 },
            "advanced": { "maxConcurrentReviews": 9, "logLevel": "debug" },
            "aggregated": true
        }))
        .unwrap();
        store
            .save_setting(UI_SETTING_KEY, &serde_json::to_value(&ui).unwrap())
            .await
            .unwrap();
        let overrides = {
            let mut map = crate::config::ExpertOverrides::default();
            map.record(
                "Security",
                ExpertOverride {
                    weight: Some(30),
                    prompt: Some("webui prompt".to_string()),
                    ..Default::default()
                },
            );
            map
        };
        store
            .save_setting(
                super::super::persist::EXPERT_OVERRIDES_KEY,
                &serde_json::to_value(&overrides).unwrap(),
            )
            .await
            .unwrap();

        let mut config = file_config();
        let applied = apply_db_overrides(&mut config, &store, &UiStateEnvOverrides::default())
            .await
            .unwrap();

        assert!(!applied.is_empty());
        // LLM chain: replaced wholesale by the DB's providers.
        assert_eq!(config.llm.len(), 1);
        assert_eq!(config.llm[0].provider, "anthropic");
        assert_eq!(config.llm[0].api_key, "sk-db");
        assert_eq!(config.llm[0].disable_thinking, Some(true));
        assert_eq!(applied.llm.len(), 1);
        // Platforms + legacy credentials: reported for the caller to own.
        assert_eq!(applied.git_platforms.len(), 1);
        assert_eq!(applied.git_platforms[0].token, "glpat-db");
        assert_eq!(applied.gitlab.token, "glpat-legacy-db");
        assert_eq!(applied.gitlab.webhook_secret, "wh-legacy-db");
        // `ui` row → caps + aggregation flag; rules have no AppConfig field.
        assert_eq!(config.max_concurrent_llm_calls, Some(9));
        assert_eq!(config.max_team_size, Some(9));
        assert!(config.report.aggregated);
        assert_eq!(applied.ui.as_ref().unwrap().rules.min_score, 91);
        assert_eq!(applied.ui.as_ref().unwrap().advanced.log_level, "debug");
        // Expert override patched the file entry, fields it did not cover kept.
        assert_eq!(config.review_experts["Security"].weight, 30);
        assert_eq!(
            config.review_experts["Security"].prompt.as_deref(),
            Some("webui prompt")
        );
        assert!(config.review_experts["Security"].enabled);
        assert_eq!(config.review_experts["Quality"].weight, 50);
        assert_eq!(applied.experts_patched, 1);
    }

    /// A key the DB does not carry keeps the TOML value — policy values that
    /// live only in the config file are never erased by a partial DB.
    #[tokio::test]
    async fn absent_database_key_keeps_the_toml_value() {
        let store = fresh_db().await;
        // The DB carries an LLM chain and nothing else: no `ui` row, no
        // experts row, no platforms, no legacy gitlab row.
        store
            .replace_llm_providers(&[db_llm("ollama", "llama3", "sk-db")])
            .await
            .unwrap();

        let mut config = file_config();
        let applied = apply_db_overrides(&mut config, &store, &UiStateEnvOverrides::default())
            .await
            .unwrap();

        assert_eq!(config.llm[0].provider, "ollama");
        assert_eq!(applied.llm.len(), 1);
        // Untouched: the audit-preset values and the expert team.
        assert_eq!(config.report.aggregated, false);
        assert_eq!(config.report.max_findings_per_expert, 7);
        assert_eq!(config.max_concurrent_llm_calls, Some(2));
        assert_eq!(config.max_team_size, Some(2));
        assert_eq!(config.review_experts["Security"].weight, 50);
        assert!(applied.ui.is_none());
        assert!(applied.git_platforms.is_empty());
        assert_eq!(applied.gitlab.token, "");
        assert_eq!(applied.experts_patched, 0);
    }

    /// A stored provider whose key is empty never reaches the chain, and does
    /// not count as "the DB carries a chain" either.
    #[tokio::test]
    async fn provider_rows_without_a_key_do_not_override_the_chain() {
        let store = fresh_db().await;
        store
            .replace_llm_providers(&[db_llm("openai", "gpt-4o-mini", "")])
            .await
            .unwrap();

        let mut config = file_config();
        let applied = apply_db_overrides(&mut config, &store, &UiStateEnvOverrides::default())
            .await
            .unwrap();

        assert!(applied.llm.is_empty());
        assert_eq!(config.llm.len(), 1);
        assert_eq!(config.llm[0].api_key, "sk-from-toml");
    }

    /// A stored key that is the `***` MASK is not a key either: the table is
    /// not usable, so the TOML chain stands and the sentinel never reaches a
    /// provider as a literal credential. Same rule as the empty-key case above
    /// — `apply_ui_config`'s `is_blank_or_masked`.
    #[tokio::test]
    async fn stored_masked_key_is_not_a_provider() {
        let store = fresh_db().await;
        store
            .replace_llm_providers(&[
                db_llm("openai", "gpt-4o", crate::models::API_KEY_MASK),
                db_llm("anthropic", "claude-3-opus", "sk-db"),
            ])
            .await
            .unwrap();

        let mut config = file_config();
        let applied = apply_db_overrides(&mut config, &store, &UiStateEnvOverrides::default())
            .await
            .unwrap();

        // The masked row is gone from the chain, the live one is the chain.
        let chain: Vec<&str> = applied.llm.iter().map(|c| c.provider.as_str()).collect();
        assert_eq!(chain, ["anthropic"]);
        assert!(applied.llm.iter().all(|c| c.api_key != crate::models::API_KEY_MASK));
        assert!(config.llm.iter().all(|c| c.api_key != crate::models::API_KEY_MASK));
        // And with ONLY the masked row, the file chain is left in place
        // rather than being replaced by an unusable one.
        let mask_only = fresh_db().await;
        mask_only
            .replace_llm_providers(&[db_llm("openai", "gpt-4o", crate::models::API_KEY_MASK)])
            .await
            .unwrap();
        let mut config = file_config();
        let applied = apply_db_overrides(&mut config, &mask_only, &UiStateEnvOverrides::default())
            .await
            .unwrap();
        assert!(applied.llm.is_empty());
        assert_eq!(config.llm.len(), 1);
        assert_eq!(config.llm[0].api_key, "sk-from-toml");
    }

    /// An env-seeded chain wins wholesale: the persisted table is not applied.
    #[tokio::test]
    async fn env_supplied_chain_wins_wholesale() {
        let store = fresh_db().await;
        store
            .replace_llm_providers(&[db_llm("anthropic", "claude-3-opus", "sk-db")])
            .await
            .unwrap();

        let env = UiStateEnvOverrides {
            llm_from_env: true,
            ..Default::default()
        };
        let mut config = file_config();
        let applied = apply_db_overrides(&mut config, &store, &env).await.unwrap();

        assert!(applied.llm.is_empty());
        assert_eq!(config.llm[0].provider, "openai");
        assert_eq!(config.llm[0].api_key, "sk-from-toml");
    }

    /// The env/CLI fallback fills a legacy credential the DB leaves empty, and
    /// never overrides one the DB carries.
    #[tokio::test]
    async fn env_fallback_fills_only_empty_legacy_credentials() {
        let store = fresh_db().await;
        store
            .save_legacy_gitlab(&PersistedGitlabConfig {
                token: "glpat-db".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();

        let env = UiStateEnvOverrides {
            gitlab_token: Some("glpat-env".to_string()),
            gitlab_webhook_secret: Some("wh-env".to_string()),
            ..Default::default()
        };
        let mut config = file_config();
        let applied = apply_db_overrides(&mut config, &store, &env).await.unwrap();

        assert_eq!(applied.gitlab.token, "glpat-db", "the DB value is authoritative");
        assert_eq!(
            applied.gitlab.webhook_secret, "wh-env",
            "the fallback fills a field the DB leaves empty"
        );
        assert_eq!(applied.gitlab.webhook_signing_secret, "");
    }

    /// `enc:` values are decrypted with the CONFIG DIR's `secrets.key` — the
    /// key `serve` resolves for the same directory.
    #[tokio::test]
    async fn enc_values_are_decrypted_with_the_config_dirs_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqlxStore::connect_default(dir.path()).await.unwrap();
        store.migrate().await.unwrap();
        store
            .replace_git_platforms(&[db_platform("testbed", "glpat-live")])
            .await
            .unwrap();
        store
            .replace_llm_providers(&[db_llm("anthropic", "claude-3-opus", "sk-live-db")])
            .await
            .unwrap();

        // Precondition: the values really are encrypted at rest, and the key
        // file lives next to `review.db`.
        let at_rest: String = sqlx::query_scalar("SELECT token FROM git_platforms")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert!(
            at_rest.starts_with("enc:"),
            "token must be encrypted at rest: {at_rest}"
        );
        assert!(dir.path().join(crate::config::secrets::SECRETS_KEY_FILE_NAME).exists());

        let mut config = file_config();
        let applied = apply_db_overrides(&mut config, &store, &UiStateEnvOverrides::default())
            .await
            .unwrap();
        assert_eq!(applied.git_platforms[0].token, "glpat-live");
        assert_eq!(config.llm[0].api_key, "sk-live-db");

        // A fresh store over the SAME config dir (what `serve` builds at
        // startup, and what the next process builds after a restart) decrypts
        // the same rows: the key is the directory's, not the process's.
        let reopened = SqlxStore::connect_default(dir.path()).await.unwrap();
        let mut config = file_config();
        let applied = apply_db_overrides(&mut config, &reopened, &UiStateEnvOverrides::default())
            .await
            .unwrap();
        assert_eq!(applied.git_platforms[0].token, "glpat-live");
        assert_eq!(config.llm[0].api_key, "sk-live-db");
    }

    /// An unusable `experts` row degrades to "no overrides" and never stops
    /// the other surfaces from applying.
    #[tokio::test]
    async fn unusable_experts_row_does_not_block_the_other_surfaces() {
        let store = fresh_db().await;
        store
            .replace_llm_providers(&[db_llm("anthropic", "claude-3-opus", "sk-db")])
            .await
            .unwrap();
        // A hand-edited row that is not JSON at all: `load_setting` fails.
        sqlx::query("INSERT INTO app_settings (key, value, updated_at) VALUES (?, ?, ?)")
            .bind(super::super::persist::EXPERT_OVERRIDES_KEY)
            .bind("this is not json")
            .bind("2026-01-01T00:00:00Z")
            .execute(store.pool())
            .await
            .unwrap();

        let mut config = file_config();
        let applied = apply_db_overrides(&mut config, &store, &UiStateEnvOverrides::default())
            .await
            .unwrap();

        assert_eq!(applied.experts_patched, 0);
        assert_eq!(config.review_experts["Security"].weight, 50, "the file values stand");
        assert_eq!(config.llm[0].provider, "anthropic", "the other surfaces still applied");
    }

    /// An override naming an expert the file does not define patches nothing
    /// and is not reported as applied — the config file stays the base.
    #[tokio::test]
    async fn override_for_an_unknown_expert_patches_nothing() {
        let store = fresh_db().await;
        let mut map = crate::config::ExpertOverrides::default();
        map.record(
            "Ghost",
            ExpertOverride {
                enabled: Some(false),
                ..Default::default()
            },
        );
        store
            .save_setting(
                super::super::persist::EXPERT_OVERRIDES_KEY,
                &serde_json::to_value(&map).unwrap(),
            )
            .await
            .unwrap();

        let mut config = file_config();
        let applied = apply_db_overrides(&mut config, &store, &UiStateEnvOverrides::default())
            .await
            .unwrap();

        assert_eq!(applied.experts_patched, 0);
        assert!(applied.is_empty(), "an override that patched nothing is not 'applied'");
        assert_eq!(config.review_experts.len(), 2);
        assert!(!config.review_experts.contains_key("Ghost"));
        // The file's own experts are untouched.
        let security: &ExpertTomlDef = &config.review_experts["Security"];
        assert!(security.enabled);
    }

    /// The returned chain is the server's authoritative order (RENG-55): the
    /// persisted `ui.llm.primaryProvider` heads it, the rest keep their stored
    /// order, and DISABLED entries are left out (RENG-75). A CLI caller that
    /// runs reviews straight off `AppliedDbOverrides::llm` therefore runs the
    /// same head provider the Web UI shows as primary.
    #[tokio::test]
    async fn stored_primary_provider_heads_the_returned_chain() {
        let store = fresh_db().await;
        let mut disabled = db_llm("gemini", "gemini-2.0", "sk-gemini");
        disabled.disabled = true;
        store
            .replace_llm_providers(&[
                db_llm("openai", "gpt-4o", "sk-openai"),
                db_llm("anthropic", "claude-3-opus", "sk-anthropic"),
                disabled,
            ])
            .await
            .unwrap();
        let ui: UiConfig =
            serde_json::from_value(serde_json::json!({ "llm": { "primaryProvider": "anthropic" } })).unwrap();
        store
            .save_setting(UI_SETTING_KEY, &serde_json::to_value(&ui).unwrap())
            .await
            .unwrap();

        let mut config = file_config();
        let applied = apply_db_overrides(&mut config, &store, &UiStateEnvOverrides::default())
            .await
            .unwrap();

        let chain: Vec<&str> = applied.llm.iter().map(|c| c.provider.as_str()).collect();
        assert_eq!(chain, ["anthropic", "openai"], "primary first, disabled excluded");
        let on_config: Vec<&str> = config.llm.iter().map(|c| c.provider.as_str()).collect();
        assert_eq!(
            on_config, chain,
            "the field doc promises config.llm and applied.llm are the same chain"
        );
    }

    /// Without a `ui` row there is no persisted primary, so the stored order of
    /// the enabled entries is already authoritative (`ordered_llm_configs("")`).
    #[tokio::test]
    async fn no_stored_primary_keeps_the_stored_chain_order() {
        let store = fresh_db().await;
        store
            .replace_llm_providers(&[
                db_llm("openai", "gpt-4o", "sk-openai"),
                db_llm("anthropic", "claude-3-opus", "sk-anthropic"),
            ])
            .await
            .unwrap();

        let mut config = file_config();
        let applied = apply_db_overrides(&mut config, &store, &UiStateEnvOverrides::default())
            .await
            .unwrap();

        let chain: Vec<&str> = applied.llm.iter().map(|c| c.provider.as_str()).collect();
        assert_eq!(chain, ["openai", "anthropic"]);
    }

    /// RENG-95 tri-state: a `ui` row written BEFORE the aggregation toggle
    /// existed carries no `aggregated` key, and the config file's value must
    /// then stay in force rather than be reset to a serde default.
    #[tokio::test]
    async fn ui_row_without_the_aggregation_toggle_keeps_the_file_flag() {
        let store = fresh_db().await;
        let ui: UiConfig =
            serde_json::from_value(serde_json::json!({ "advanced": { "maxConcurrentReviews": 9 } })).unwrap();
        assert!(ui.aggregated.is_none(), "precondition: the row predates the toggle");
        store
            .save_setting(UI_SETTING_KEY, &serde_json::to_value(&ui).unwrap())
            .await
            .unwrap();

        // The file says `aggregated = false` (see `file_config`)…
        let mut config = file_config();
        let applied = apply_db_overrides(&mut config, &store, &UiStateEnvOverrides::default())
            .await
            .unwrap();
        assert!(!config.report.aggregated);
        // …and `true` on the other side of the "did the row decide it" line:
        // a silent row never resets the file value either way.
        config.report.aggregated = true;
        apply_db_overrides(&mut config, &store, &UiStateEnvOverrides::default())
            .await
            .unwrap();
        assert!(config.report.aggregated, "the file value stays; the row is silent");
        assert_eq!(config.max_concurrent_llm_calls, Some(9), "the row still applies");
        assert!(applied.ui.is_some());
    }

    /// `Debug` must never print a live credential — the result is a natural
    /// thing to `{:?}`-log at startup.
    #[tokio::test]
    async fn debug_output_redacts_every_secret() {
        let store = fresh_db().await;
        store
            .replace_llm_providers(&[db_llm("anthropic", "claude-3-opus", "sk-super-secret")])
            .await
            .unwrap();
        store
            .replace_git_platforms(&[db_platform("testbed", "glpat-super-secret")])
            .await
            .unwrap();
        store
            .save_legacy_gitlab(&PersistedGitlabConfig {
                token: "glpat-legacy-secret".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();

        let mut config = file_config();
        let applied = apply_db_overrides(&mut config, &store, &UiStateEnvOverrides::default())
            .await
            .unwrap();

        let rendered = format!("{applied:?}");
        for secret in ["sk-super-secret", "glpat-super-secret", "glpat-legacy-secret"] {
            assert!(!rendered.contains(secret), "{secret} leaked into Debug: {rendered}");
        }
        // What a diagnostic needs is still there.
        assert!(rendered.contains("anthropic:claude-3-opus"), "{rendered}");
        assert!(rendered.contains("testbed"), "{rendered}");
        assert!(rendered.contains("token=set"), "{rendered}");
    }
}
