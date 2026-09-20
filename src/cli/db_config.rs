//! The configuration a CLI command runs on: the config-file chain, then the
//! database (RENG-107).
//!
//! ```text
//! built-in defaults → env → ~/.config/review-engine/.code-audit-config.toml
//!                    → ./.code-audit-config.toml     ← `config::resolve_config`
//!                    → review.db (the state root)    ← `db_overlay::apply_db_overrides`
//! ```
//!
//! Every CLI entry point that runs a review, a description, an improvement, a
//! question or a changelog goes through [`resolve_cli_config`], so a manual
//! `reng review` uses the same LLM providers, expert overrides and ui settings
//! the Web UI wrote — the user-visible half of "the database is the
//! highest-priority configuration layer". `serve` is the one caller that does
//! NOT come through here: its bootstrap resolves the file chain itself and then
//! applies the same overlay (plus the WebUI replay on top), which is what the
//! shared `apply_db_overrides` is for. The layer above does not change: policy
//! values that exist only in the config file (`[report]`, `[scoring]`, the
//! language profiles) survive, because the database overrides key by key.
//!
//! The root is whatever `--config-dir` / `serve --data-dir` / `REVIEW_DATA_DIR`
//! / `REVIEW_ENGINE_CONFIG_DIR` resolved before the command ran, derived
//! exactly as `serve` derives it (see [`resolve_cli_config`]), so the CLI reads
//! the `review.db` the Web UI writes. `DATABASE_URL` (PostgreSQL) is NOT
//! consulted — the CLI's configuration database is the state root's
//! `review.db`, and a deployment that runs its configuration on a server has
//! no file for the CLI to read.
//!
//! ## Reading the database can never be fatal
//!
//! The rules, in the order a user meets them:
//!
//! - no `review.db`, or `REVIEW_DISABLE_DB=1` → plain TOML, silently: this is
//!   the pre-0.10 behaviour and nothing about it is worth a warning.
//! - `review.db` that cannot be opened, whose rows cannot be read, or whose
//!   `secrets.key` is missing → ONE warning on stderr and the TOML
//!   resolution stands. A configuration read never blocks the command.
//!
//! The warning is `eprintln!` rather than `tracing::warn!` for the reason
//! `handlers::config::provider` documents: main routes tracing into
//! `logs.ndjson` and the Web UI's ring buffer, so a tracing warning does not
//! reach the terminal a `reng review` is running in. `AppliedDbOverrides` is
//! already redacted for `Debug`, so nothing here can print a secret.

use std::path::Path;

use review_engine::models::{AppConfig, ConfigSource};
use review_engine::server::api::config::db_overlay::{apply_db_overrides, AppliedDbOverrides};
use review_engine::server::api::config::persist::{db_disabled_flag, UiStateEnvOverrides};
use review_engine::store::SqlxStore;

/// A command's resolved configuration and what the database contributed.
///
/// `config` is the config-file resolution with the database's own surfaces
/// applied on top — the [`AppConfig`] the command runs on. `db` is what the
/// database carried: the fields with an `AppConfig` counterpart are already
/// applied to `config`, the rest (the git platforms, the legacy GitLab
/// credentials, the ui projection) have no place on `AppConfig` and are only
/// reported here. It is empty when the database carried nothing.
pub struct CliConfig {
    pub config: AppConfig,
    pub db: AppliedDbOverrides,
}

impl CliConfig {
    /// The configuration a command runs on, recording what the database
    /// contributed first.
    ///
    /// The notice is the CLI's half of what `serve` prints when it overlays the
    /// database at startup, and it answers the question the priority rule
    /// invites ("why is this run using a provider I never put in the config
    /// file?"): the answer ends up in the state root's `logs.ndjson`. The
    /// `AppliedDbOverrides` value is logged through its own `Debug`, which is
    /// written to redact every secret — a live key must never reach a log.
    pub fn into_config(self) -> AppConfig {
        if !self.db.is_empty() {
            tracing::info!(db = ?self.db, "the config database overrode the config files for this command");
        }
        self.config
    }
}

/// Resolve a CLI command's configuration: the config-file chain, then the
/// state root's database when it exists.
///
/// This is the ONE path the config-consuming entry points (`review`,
/// `repo-review`, `improve`, `ask`, `describe`, `update-changelog`) take, so
/// they cannot drift apart on DB priority — and so `docs/configuration.md`'s
/// "the CLI applies the database over the config file the same way `serve`
/// does" stays true for every command that resolves a configuration.
///
/// The root is derived the way `serve` derives it — the directory holding
/// `ui-state.toml` ([`resolve_ui_state_path`](review_engine::server::api::config::persist::resolve_ui_state_path)),
/// which is the state root for every default and follows
/// `REVIEW_UI_STATE_FILE` when a deployment moved that artifact. `--config-dir`
/// has already been applied as the process data dir by then, so the flag, the
/// environment and the per-artifact overrides all resolve exactly as they do
/// for the server: the CLI reads the same `review.db`, with the same
/// `secrets.key` next to it.
pub async fn resolve_cli_config(config_source: Option<ConfigSource>) -> anyhow::Result<CliConfig> {
    let root = review_engine::server::api::config::persist::resolve_ui_state_path()
        .and_then(|state_file| state_file.parent().map(Path::to_path_buf));
    resolve_cli_config_at(config_source, root.as_deref()).await
}

/// [`resolve_cli_config`] against an explicit state root.
///
/// The root is a parameter rather than read from the process global inside, so
/// tests can point a resolution at a temp directory without mutating it for
/// every other test in the binary.
pub async fn resolve_cli_config_at(
    config_source: Option<ConfigSource>,
    state_root: Option<&Path>,
) -> anyhow::Result<CliConfig> {
    let mut config = review_engine::config::resolve_config(config_source).await?;
    clear_unusable_caps(&mut config);
    let db = overlay_database(&mut config, state_root).await;
    Ok(CliConfig { config, db })
}

/// Clear a concurrency cap of 0 that the resolved config carries, warning once.
///
/// `resolve_config(None)` — the auto-detected `.code-audit-config.toml` — only
/// lifts `llm` / `report` / `commands` / `review_experts` out of the file
/// (`config::resolver`), so a top-level `max_concurrent_llm_calls` written there
/// never arrives. `--config <file>` (and the library's `ConfigSource::Path` /
/// `Inline`) deserializes the whole `AppConfig` instead, so the same line DOES
/// arrive — and 0 is not a limit: the review pipeline builds
/// `Semaphore::new(cap)` from it, so every expert task waits forever and the
/// command hangs with no error, no timeout and no last log line. `--config` is
/// on every config-consuming command, so the guard belongs here, on the one
/// resolution those commands share; `team::orchestrator::concurrent_llm_calls`
/// is the sink-side backstop for callers that never come through the CLI.
///
/// Clearing, rather than clamping to 1, gives the file's 0 the meaning the two
/// database writers' rule gives it: "not decided", so the pipeline's own
/// default applies. This runs BEFORE the database overlay, so a stored
/// `advanced.maxConcurrentReviews` still gets to decide the caps afterward.
fn clear_unusable_caps(config: &mut AppConfig) {
    let ignored: Vec<&str> = [
        ("max_concurrent_llm_calls", config.max_concurrent_llm_calls),
        ("max_team_size", config.max_team_size),
    ]
    .into_iter()
    .filter(|(_, value)| *value == Some(0))
    .map(|(name, _)| name)
    .collect();
    if ignored.is_empty() {
        return;
    }
    config.max_concurrent_llm_calls = config.max_concurrent_llm_calls.filter(|cap| *cap > 0);
    config.max_team_size = config.max_team_size.filter(|cap| *cap > 0);
    // Same channel as the database warning below, for the same reason: main
    // routes tracing into `logs.ndjson`, so a tracing warning never reaches the
    // terminal this command is running in.
    eprintln!(
        "warning: ignoring {} = 0 in the configuration file — no expert task can run with a \
         limit of 0, so the built-in default applies",
        ignored.join(" / ")
    );
}

/// Apply the state root's database over `config`, degrading to plain TOML on
/// every failure. See the module docs for the contract.
async fn overlay_database(config: &mut AppConfig, state_root: Option<&Path>) -> AppliedDbOverrides {
    let Some(root) = state_root else {
        return AppliedDbOverrides::default();
    };
    // The 0.9 escape hatch, read exactly as `serve` reads it: an operator who
    // disabled persistence must not have the CLI consult it either.
    if db_disabled_flag(std::env::var("REVIEW_DISABLE_DB").ok().as_deref()) {
        return AppliedDbOverrides::default();
    }
    let db_path = root.join(review_engine::paths::DB_FILE_NAME);
    if !db_path.is_file() {
        // No database: the config files ARE the configuration. Silent, and
        // byte-identical to the pre-database behaviour.
        return AppliedDbOverrides::default();
    }
    let store = match SqlxStore::connect_default_readonly(root).await {
        Ok(store) => store,
        Err(e) => {
            warn_config_files_win(&db_path, &e);
            return AppliedDbOverrides::default();
        }
    };
    // The CLI has no env/CLI tracking to hand over, and it must not re-order
    // the DB chain: `UiStateEnvOverrides::default()` means "no env-seeded
    // chain", so the persisted providers are the ones applied, in the server's
    // own chain order, and the legacy-credential fallback stays unused (the
    // CLI's git tokens come from --gitlab-token / GITLAB_TOKEN).
    match apply_db_overrides(config, &store, &UiStateEnvOverrides::default()).await {
        Ok(applied) => applied,
        Err(e) => {
            // `apply_db_overrides` is atomic on error — the config is still the
            // TOML resolution — so "the database carries no configuration" is
            // exactly right here.
            warn_config_files_win(&db_path, &e);
            AppliedDbOverrides::default()
        }
    }
}

/// The one warning the degrade path prints: which database was ignored and
/// why, and what the command is running on instead.
fn warn_config_files_win(db_path: &Path, err: &anyhow::Error) {
    eprintln!(
        "warning: ignoring the configuration database {} — using the config files instead: {err:#}",
        db_path.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    use review_engine::config::ExpertOverride;
    use review_engine::models::LLMConfig;
    use review_engine::server::api::config::persist::{PersistedGitlabConfig, EXPERT_OVERRIDES_KEY, UI_SETTING_KEY};
    use review_engine::server::api::config::UiConfig;
    use review_engine::store::traits::ConfigStore;
    use std::sync::{Mutex, MutexGuard};

    /// The env vars that reach `resolve_config` / the overlay from the outside.
    /// Serialized through `ENV_LOCK`, following the guard pattern of
    /// `config::resolver::tests` and `handlers::config::tests`.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn unset(key: &'static str) -> Self {
            let original = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, original }
        }

        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.original.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    /// The variables the shell of whoever runs the tests must not leak into a
    /// resolution: an env-seeded LLM chain wins over the TOML one wholesale.
    fn clean_env() -> (MutexGuard<'static, ()>, Vec<EnvGuard>) {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        (
            lock,
            vec![EnvGuard::unset("LLM_CONFIG"), EnvGuard::unset("REVIEW_DISABLE_DB")],
        )
    }

    /// The config-file side: one provider, a couple of the built-in experts
    /// (weights re-balanced so the enabled ones still sum to 100 — the file
    /// resolution validates that before any database is consulted), a `[report]`
    /// policy value and an `aggregated` flag. These are the values a database
    /// that does not carry them must leave alone.
    fn file_toml() -> String {
        r#"
[report]
max_findings_per_expert = 7
aggregated = false

[review_experts.security]
weight = 30

[review_experts.lead]
weight = 5

[[llm]]
provider = "openai"
model = "gpt-4o"
api_base = "https://api.openai.com/v1"
api_key = "sk-from-toml"
"#
        .to_string()
    }

    fn inline_source() -> Option<ConfigSource> {
        Some(ConfigSource::Inline(file_toml()))
    }

    fn db_card(provider: &str, model: &str, key: &str) -> LLMConfig {
        LLMConfig {
            provider: provider.to_string(),
            model: model.to_string(),
            api_key: key.to_string(),
            api_base: "https://api.db.example/v1".to_string(),
            max_tokens: 8192,
            temperature: 0.4,
            disable_thinking: None,
            disabled: false,
        }
    }

    /// A state root seeded the way `serve` seeds it: migrate, then rows whose
    /// secrets are encrypted at rest with the directory's `secrets.key`.
    async fn seeded_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let store = SqlxStore::connect_default(dir.path()).await.unwrap();
        store.migrate().await.unwrap();
        store
            .replace_llm_providers(&[db_card("anthropic", "claude-3-opus", "sk-from-db")])
            .await
            .unwrap();
        store
            .replace_git_platforms(&[review_engine::models::GitPlatformConfig {
                name: "testbed".to_string(),
                platform_type: "gitlab".to_string(),
                base_url: "http://gitlab.internal:8929".to_string(),
                token: "glpat-from-db".to_string(),
                webhook_secret: "wh-from-db".to_string(),
                ..Default::default()
            }])
            .await
            .unwrap();
        {
            let mut overrides = review_engine::config::ExpertOverrides::default();
            overrides.record(
                "security",
                ExpertOverride {
                    weight: Some(45),
                    ..Default::default()
                },
            );
            store
                .save_setting(EXPERT_OVERRIDES_KEY, &serde_json::to_value(&overrides).unwrap())
                .await
                .unwrap();
        }
        let ui: UiConfig = serde_json::from_value(serde_json::json!({
            "rules": { "minScore": 91 },
            "advanced": { "maxConcurrentReviews": 9 },
            "aggregated": true
        }))
        .unwrap();
        store
            .save_setting(UI_SETTING_KEY, &serde_json::to_value(&ui).unwrap())
            .await
            .unwrap();
        store.pool().close().await;
        dir
    }

    /// The whole point of the mission: a manual `reng review` runs on the
    /// providers the Web UI stored, decrypted with the state root's
    /// `secrets.key`, while every value only the config file carries survives.
    #[tokio::test]
    async fn database_rows_override_the_config_files() {
        let _env_lock = clean_env();
        let root = seeded_root().await;

        let resolved = resolve_cli_config_at(inline_source(), Some(root.path())).await.unwrap();

        // LLM chain: the DB's provider, with the decrypted key.
        let chain: Vec<(&str, &str)> = resolved
            .config
            .llm
            .iter()
            .map(|c| (c.provider.as_str(), c.api_key.as_str()))
            .collect();
        assert_eq!(chain, [("anthropic", "sk-from-db")]);
        assert_eq!(resolved.db.llm.len(), 1, "the DB reported the chain it applied");
        // Expert override: the DB's weight on the expert it names, the file's
        // values on every expert it does not.
        assert_eq!(resolved.config.review_experts["security"].weight, 45);
        assert!(
            resolved.config.review_experts["security"].enabled,
            "a field the override does not cover keeps the config-file value"
        );
        assert_eq!(resolved.config.review_experts["lead"].weight, 5);
        assert_eq!(resolved.db.experts_patched, 1);
        // ui row → the two concurrency caps and the aggregation flag.
        assert_eq!(resolved.config.max_concurrent_llm_calls, Some(9));
        assert_eq!(resolved.config.max_team_size, Some(9));
        assert!(resolved.config.report.aggregated);
        assert_eq!(resolved.db.ui.as_ref().unwrap().rules.min_score, 91);
        // Policy values that live only in the file are untouched.
        assert_eq!(resolved.config.report.max_findings_per_expert, 7);
        assert_eq!(resolved.config.review_experts["quality"].weight, 10);
    }

    /// The surfaces `AppConfig` has no field for are CARRIED, not dropped: the
    /// git platforms (live, decrypted secrets) and the legacy GitLab
    /// credentials come back on the resolution result. Consuming them for the
    /// CLI's own API calls is deliberately NOT wired (the CLI keeps
    /// `--gitlab-token` / `GITLAB_TOKEN`), but a caller must be able to see
    /// what the database holds.
    #[tokio::test]
    async fn database_only_surfaces_are_carried_not_dropped() {
        let _env_lock = clean_env();
        let root = seeded_root().await;

        let resolved = resolve_cli_config_at(inline_source(), Some(root.path())).await.unwrap();

        assert!(!resolved.db.is_empty());
        assert_eq!(resolved.db.git_platforms.len(), 1);
        assert_eq!(resolved.db.git_platforms[0].name, "testbed");
        assert_eq!(
            resolved.db.git_platforms[0].token, "glpat-from-db",
            "the platform token must arrive decrypted"
        );
        // The token really is encrypted at rest, so the value above went
        // through the state root's `secrets.key` rather than a plain-text
        // passthrough.
        let store = SqlxStore::connect_default_readonly(root.path()).await.unwrap();
        let at_rest: String = ::sqlx::query_scalar("SELECT token FROM git_platforms")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert!(at_rest.starts_with("enc:"), "at rest: {at_rest}");
    }

    /// RENG-55 / RENG-75 order, consumed exactly as the server produces it: the
    /// persisted `ui.llm.primaryProvider` heads the chain, the rest keep the
    /// stored order, disabled entries are out. This is what makes `reng review`
    /// use the same head provider the Web UI shows as primary.
    #[tokio::test]
    async fn the_chain_order_is_the_servers() {
        let _env_lock = clean_env();
        let dir = tempfile::tempdir().unwrap();
        let store = SqlxStore::connect_default(dir.path()).await.unwrap();
        store.migrate().await.unwrap();
        let mut disabled = db_card("gemini", "gemini-2.0", "sk-gemini");
        disabled.disabled = true;
        store
            .replace_llm_providers(&[
                db_card("openai", "gpt-4o", "sk-openai"),
                db_card("anthropic", "claude-3-opus", "sk-anthropic"),
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
        store.pool().close().await;

        let resolved = resolve_cli_config_at(inline_source(), Some(dir.path())).await.unwrap();

        let chain: Vec<&str> = resolved.config.llm.iter().map(|c| c.provider.as_str()).collect();
        assert_eq!(chain, ["anthropic", "openai"], "primary first, disabled excluded");
        let reported: Vec<&str> = resolved.db.llm.iter().map(|c| c.provider.as_str()).collect();
        assert_eq!(reported, chain, "the reported chain is the one on the config");
    }

    /// No database → the config-file resolution, byte for byte, and nothing
    /// reported as applied. This is the pre-0.10 behaviour every existing test
    /// and deployment keeps.
    #[tokio::test]
    async fn without_a_database_the_file_resolution_stands() {
        let _env_lock = clean_env();
        let dir = tempfile::tempdir().unwrap();

        let resolved = resolve_cli_config_at(inline_source(), Some(dir.path())).await.unwrap();

        assert!(resolved.db.is_empty());
        assert_eq!(resolved.config.llm.len(), 1);
        assert_eq!(resolved.config.llm[0].api_key, "sk-from-toml");
        assert_eq!(resolved.config.review_experts["security"].weight, 30);
        assert_eq!(resolved.config.max_concurrent_llm_calls, None);
        assert!(!resolved.config.report.aggregated);
    }

    /// A resolution with no state root at all (a degraded environment with no
    /// `HOME`) has no database to consult — and must not panic.
    #[tokio::test]
    async fn no_state_root_means_no_database_lookup() {
        let _env_lock = clean_env();

        let resolved = resolve_cli_config_at(inline_source(), None).await.unwrap();

        assert!(resolved.db.is_empty());
        assert_eq!(resolved.config.llm[0].api_key, "sk-from-toml");
    }

    /// RENG-107 r2 — the hang the reviewer found. `--config <file>` (and the
    /// library's `ConfigSource::Path` / `Inline`) deserializes the whole
    /// `AppConfig`, so a top-level concurrency cap of 0 in THAT file does
    /// arrive in the resolved config — unlike the auto-detected
    /// `.code-audit-config.toml`, whose top-level scalars the resolver drops
    /// (`resolve_config(None)` lifts only `llm` / `report` / `commands` /
    /// `review_experts`). 0 is not a limit: the review pipeline builds
    /// `Semaphore::new(cap)` from it, so every expert task waits forever, which
    /// is how `reng review --config cfg.toml` used to hang with rc=124 and a
    /// single LLM request. The CLI clears both keys, so the pipeline default
    /// applies — and it clears them BEFORE the database overlay, so a stored
    /// cap still gets to decide.
    #[tokio::test]
    async fn a_zero_cap_from_an_explicit_config_file_is_not_a_limit() {
        let _env_lock = clean_env();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cfg.toml");
        let source = || Some(ConfigSource::Path(file.display().to_string()));

        std::fs::write(
            &file,
            format!("max_concurrent_llm_calls = 0\nmax_team_size = 0\n{}", file_toml()),
        )
        .unwrap();
        let resolved = resolve_cli_config_at(source(), None).await.unwrap();
        assert_eq!(
            resolved.config.max_concurrent_llm_calls, None,
            "0 must be cleared, not carried into the pipeline's semaphore"
        );
        assert_eq!(resolved.config.max_team_size, None);
        // Clearing must not disturb the rest of the file.
        assert_eq!(resolved.config.llm[0].provider, "openai");
        assert_eq!(resolved.config.review_experts["security"].weight, 30);

        // A real cap in the same file is honoured — the guard is not a blanket
        // reset of the key.
        std::fs::write(
            &file,
            format!("max_concurrent_llm_calls = 3\nmax_team_size = 3\n{}", file_toml()),
        )
        .unwrap();
        let resolved = resolve_cli_config_at(source(), None).await.unwrap();
        assert_eq!(resolved.config.max_concurrent_llm_calls, Some(3));
        assert_eq!(resolved.config.max_team_size, Some(3));

        // …and the clearing happens before the DB overlay: a stored cap still
        // decides for a run whose file said 0.
        let root = seeded_root().await;
        std::fs::write(&file, format!("max_concurrent_llm_calls = 0\n{}", file_toml())).unwrap();
        let resolved = resolve_cli_config_at(source(), Some(root.path())).await.unwrap();
        assert_eq!(
            resolved.config.max_concurrent_llm_calls,
            Some(9),
            "the database's cap applies over the cleared file value"
        );
    }

    /// The database is looked for exactly where `serve` keeps it: next to the
    /// `ui-state.toml` the run resolves. A deployment that moved that artifact
    /// with `REVIEW_UI_STATE_FILE` (the documented escape hatch — the database
    /// and `secrets.key` move with it) gets the same database on both sides,
    /// instead of a CLI that silently reads the config files.
    #[tokio::test]
    async fn the_database_is_read_where_the_ui_state_file_points() {
        let _env_lock = clean_env();
        let root = seeded_root().await;
        let moved = root.path().join("moved-ui-state.toml");
        let _ui_state = EnvGuard::set("REVIEW_UI_STATE_FILE", moved.to_str().unwrap());

        let resolved = resolve_cli_config(inline_source()).await.unwrap();

        assert_eq!(
            resolved.config.llm[0].provider, "anthropic",
            "the relocated state root's database must be the one applied"
        );
        assert_eq!(resolved.config.llm[0].api_key, "sk-from-db");
    }

    /// A `review.db` that cannot be read (a truncated copy, a foreign file) is
    /// not a failure: the config files win, nothing is reported as applied —
    /// and the read must not have created a sidecar or a key file along the
    /// way (RENG-105/106).
    #[tokio::test]
    async fn an_unreadable_database_degrades_to_the_file_resolution() {
        let _env_lock = clean_env();
        let dir = tempfile::tempdir().unwrap();
        // A key file must exist, or the open would stop before the database.
        std::fs::write(
            dir.path().join(review_engine::config::secrets::SECRETS_KEY_FILE_NAME),
            [7u8; 32],
        )
        .unwrap();
        std::fs::write(
            dir.path().join(review_engine::paths::DB_FILE_NAME),
            b"this is not a sqlite database",
        )
        .unwrap();

        let resolved = resolve_cli_config_at(inline_source(), Some(dir.path())).await.unwrap();

        assert!(resolved.db.is_empty(), "an unreadable DB carries nothing");
        assert_eq!(resolved.config.llm[0].provider, "openai");
        assert_eq!(resolved.config.llm[0].api_key, "sk-from-toml");
        assert_eq!(resolved.config.review_experts["security"].weight, 30);
        assert!(
            !dir.path().join("review.db-wal").exists() && !dir.path().join("review.db-shm").exists(),
            "a read must never mint the WAL sidecars"
        );
    }

    /// A database whose `secrets.key` is gone cannot have its `enc:` rows read;
    /// that is a readable-database failure too, and it degrades the same way
    /// instead of generating a second key next to the server's.
    #[tokio::test]
    async fn a_database_without_its_key_degrades_to_the_file_resolution() {
        let _env_lock = clean_env();
        let root = seeded_root().await;
        std::fs::remove_file(root.path().join(review_engine::config::secrets::SECRETS_KEY_FILE_NAME)).unwrap();

        let resolved = resolve_cli_config_at(inline_source(), Some(root.path())).await.unwrap();

        assert!(resolved.db.is_empty());
        assert_eq!(resolved.config.llm[0].api_key, "sk-from-toml");
        assert!(
            !root
                .path()
                .join(review_engine::config::secrets::SECRETS_KEY_FILE_NAME)
                .exists(),
            "the read must not create a key file"
        );
    }

    /// `REVIEW_DISABLE_DB=1` is the documented escape hatch, and the CLI reads
    /// it exactly as `serve` does: the database is not consulted at all.
    #[tokio::test]
    async fn review_disable_db_skips_the_database() {
        let _env_lock = clean_env();
        let _disabled = EnvGuard::set("REVIEW_DISABLE_DB", "1");
        let root = seeded_root().await;

        let resolved = resolve_cli_config_at(inline_source(), Some(root.path())).await.unwrap();

        assert!(resolved.db.is_empty());
        assert_eq!(resolved.config.llm[0].api_key, "sk-from-toml");
    }

    /// A stored legacy GitLab credential is DB configuration, but it is not
    /// wired into the CLI's own API calls: the flag/environment token keeps
    /// being what authenticates a review (docs/configuration.md §"the CLI's git
    /// tokens"). The resolution reports the stored value and changes nothing
    /// about how the token is obtained.
    #[tokio::test]
    async fn stored_gitlab_credentials_do_not_replace_the_cli_token() {
        let _env_lock = clean_env();
        let dir = tempfile::tempdir().unwrap();
        let store = SqlxStore::connect_default(dir.path()).await.unwrap();
        store.migrate().await.unwrap();
        store
            .save_legacy_gitlab(&PersistedGitlabConfig {
                token: "glpat-stored".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        store.pool().close().await;

        let resolved = resolve_cli_config_at(inline_source(), Some(dir.path())).await.unwrap();

        assert!(!resolved.db.is_empty(), "the stored credential is DB configuration");
        assert_eq!(resolved.db.gitlab.token, "glpat-stored");
        // The config the command runs on is untouched by it: no AppConfig field
        // exists for a GitLab token, and the CLI still reads --gitlab-token /
        // GITLAB_TOKEN at its call sites.
        assert_eq!(resolved.config.llm[0].api_key, "sk-from-toml");
    }
}
