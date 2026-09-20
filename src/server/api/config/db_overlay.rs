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
//! | `llm_providers` rows | `AppConfig::llm` (the whole chain) |
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
//! fall back (the DB carries no configuration) or to report it. The one
//! exception is the `experts` row: its read failure degrades to "no overrides"
//! with a WARN, because a hand-edited row must not be able to stop the other
//! surfaces from applying (the same contract
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
#[derive(Debug, Clone, Default)]
pub struct AppliedDbOverrides {
    /// `app_settings` row `ui` — the persisted UI projection (rules,
    /// advanced, aggregated, and the masked llm/gitPlatform sub-sections).
    /// `None` when the row does not exist. The `advanced` / `aggregated`
    /// values are applied to the config; `rules` have no `AppConfig`
    /// counterpart and are only reported.
    pub ui: Option<UiConfig>,
    /// The provider chain the DB contributed, exactly as it landed on
    /// `AppConfig::llm`. Empty when the DB carries no usable provider or when
    /// env supplied the chain.
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
}

impl AppliedDbOverrides {
    /// True when the database carried no configuration at all — the caller's
    /// config is the plain TOML resolution and nothing needs republishing.
    /// Overrides that named an expert the file does not define count as
    /// nothing: they patched no entry.
    pub fn is_empty(&self) -> bool {
        self.ui.is_none()
            && self.llm.is_empty()
            && self.git_platforms.is_empty()
            && self.gitlab.token.is_empty()
            && self.gitlab.webhook_secret.is_empty()
            && self.gitlab.webhook_signing_secret.is_empty()
            && self.experts_patched == 0
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
/// Returns an error only for a store-level failure. The `experts` row is the
/// exception (see the module docs): it degrades to "no overrides" with a WARN.
pub async fn apply_db_overrides(
    config: &mut AppConfig,
    store: &SqlxStore,
    env: &UiStateEnvOverrides,
) -> anyhow::Result<AppliedDbOverrides> {
    let mut applied = AppliedDbOverrides::default();

    // ── app_settings `ui` (rules / advanced / aggregated) ───────────────
    applied.ui = store
        .load_setting(UI_SETTING_KEY)
        .await?
        .map(|value| serde_json::from_value(value).context("app_settings row 'ui' is not a valid UiConfig"))
        .transpose()?;
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
        // `config.toml < ui-state.toml / review.db < env`: an env-seeded chain
        // wins wholesale, so the table is not applied at all.
        tracing::debug!(
            "the LLM_CONFIG/env provider chain wins over the persisted llm_providers table; \
             the database chain is not applied"
        );
    } else {
        // A stored card whose key is empty never reaches the chain — the same
        // rule `apply_ui_config` applies to a card the user emptied, so both
        // DB-apply paths agree on what "a configured provider" is.
        let usable: Vec<LLMConfig> = store
            .load_llm_providers()
            .await?
            .into_iter()
            .filter(|provider| !provider.api_key.is_empty())
            .collect();
        if !usable.is_empty() {
            config.llm = usable.clone();
            applied.llm = usable;
        }
    }

    // ── git_platforms (no `AppConfig` field: reported, not applied) ─────
    applied.git_platforms = store.load_git_platforms().await?;

    // ── app_settings `gitlab` (legacy credentials) ──────────────────────
    let stored_gitlab = store.load_legacy_gitlab().await?;
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
    match load_expert_overrides(store).await {
        Ok(overrides) if !overrides.is_empty() => {
            applied.experts_patched = overrides.apply_to(config);
            if applied.experts_patched < overrides.len() {
                tracing::debug!(
                    stored = overrides.len(),
                    applied = applied.experts_patched,
                    "expert override(s) name an expert the config file does not define; skipped"
                );
            }
        }
        Ok(_) => {}
        Err(e) => {
            // Field named `reason`, not `error`: the log collector infers a
            // plain-text line's level by substring (`infer_level_from_line`),
            // and an `error=…` field would file this WARN as an ERROR.
            tracing::warn!(
                reason = %format!("{e:#}"),
                "ignoring the persisted expert overrides: the settings row could not be read; \
                 the config file's [review_experts] values stand"
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
}
