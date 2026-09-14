//! The process-wide data directory (`serve --data-dir`, RENG-37).
//!
//! Every piece of state the server persists resolves through [`state_dir`]:
//! the SQLite database (`review.db`), its at-rest key (`secrets.key`),
//! `ui-state.toml`, `auth.toml`, the dispatcher's dedup state
//! (`dispatcher-state.json`), `feedback.json`, the models.dev cache
//! (`models-dev-cache.json`), `logs.ndjson`, the report output directory
//! (`reports/`) and the user-level `.code-audit-config.toml`.
//!
//! `serve --data-dir <path>` (or `REVIEW_DATA_DIR=<path>`) replaces the root
//! those defaults are derived from, so several instances can run side by side
//! without hacking `HOME`. Without it the resolution is byte-for-byte what it
//! was before the flag existed.
//!
//! # Precedence
//!
//! The root of the tree ([`state_dir`]), first match wins:
//!
//! 1. `--data-dir <path>` — CLI flag; `REVIEW_DATA_DIR` is its environment
//!    form and the flag wins over it;
//! 2. `REVIEW_ENGINE_CONFIG_DIR` — the override that predates the flag (the
//!    shipped images set it to `/app/config`);
//! 3. `~/.config/review-engine`, from the home directory.
//!
//! A *single* artifact can still be placed anywhere by its own full-path
//! environment variable — `REVIEW_UI_STATE_FILE`, `REVIEW_AUTH_FILE`,
//! `REVIEW_DISPATCH_STATE`, `REVIEW_FEEDBACK_PATH`,
//! `REVIEW_MODELS_DEV_CACHE` — and `DATABASE_URL` does the same for the
//! database. Those values are absolute paths written by the operator, so they
//! win over the root: existing deployments that point one artifact at a mount
//! keep working unchanged. The consequence is documented rather than hidden:
//! they are process-wide, so an instance started with `--data-dir` *and* one of
//! them set is not isolated for that artifact. [`escaping_overrides`] names
//! every such variable that is set, and `serve` logs one warning per hit at
//! startup. Two instances that each get their own `--data-dir` and a clean
//! environment share nothing.
//!
//! # Why a process global
//!
//! The resolvers sit deep inside the server (the sqlx store, the dispatcher,
//! the log collector, the auth middleware) and one of them is a serde default
//! (`output_dir`), so there is no single call site to thread a root through.
//! The root is therefore set once, at startup, before anything resolves a path
//! — [`apply_data_dir`] — and read everywhere else. [`state_dir_from`] and
//! [`resolve_artifact_at`] expose the same resolution as pure functions for
//! tests.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use anyhow::Context;

/// Environment form of `--data-dir`.
pub const DATA_DIR_ENV: &str = "REVIEW_DATA_DIR";

/// Pre-existing config-directory override; the fallback root when no data dir
/// is given.
pub const CONFIG_DIR_ENV: &str = "REVIEW_ENGINE_CONFIG_DIR";

/// Full-path override for `ui-state.toml`.
pub const UI_STATE_FILE_ENV: &str = "REVIEW_UI_STATE_FILE";

/// Full-path override for `auth.toml`.
pub const AUTH_FILE_ENV: &str = "REVIEW_AUTH_FILE";

// ─── Artifact file names (the state layout) ────────────────────────

/// Web-UI configuration + encrypted git credentials.
pub const UI_STATE_FILE_NAME: &str = "ui-state.toml";
/// Persisted API-token digest.
pub const AUTH_FILE_NAME: &str = "auth.toml";
/// Webhook dispatch dedup state.
pub const DISPATCH_STATE_FILE_NAME: &str = "dispatcher-state.json";
/// Finding-feedback log.
pub const FEEDBACK_FILE_NAME: &str = "feedback.json";
/// models.dev catalog disk cache.
pub const CATALOG_CACHE_FILE_NAME: &str = "models-dev-cache.json";
/// Structured log stream.
pub const LOG_FILE_NAME: &str = "logs.ndjson";
/// Embedded SQLite database.
pub const DB_FILE_NAME: &str = "review.db";
/// User-level config file (project-level `.code-audit-config.toml` stays in the
/// project directory — it is project input, not server state).
pub const USER_CONFIG_FILE_NAME: &str = ".code-audit-config.toml";
/// Timestamped review reports (`report.output_dir` default).
pub const REPORTS_DIR_NAME: &str = "reports";

/// One artifact that lives directly in the state dir.
pub struct StateFile {
    /// File (or directory) name inside the state dir.
    pub name: &'static str,
    /// Env var that can move this artifact to a full path of its own; empty
    /// when the artifact has no such override (see [`escaping_overrides`]).
    pub env_override: &'static str,
}

/// The state layout in one place: every artifact the server persists, with the
/// per-artifact override that can pull it out of the data dir. `--data-dir`
/// covers exactly the entries whose override is unset.
pub const STATE_FILES: &[StateFile] = &[
    StateFile {
        name: UI_STATE_FILE_NAME,
        env_override: UI_STATE_FILE_ENV,
    },
    StateFile {
        name: AUTH_FILE_NAME,
        env_override: AUTH_FILE_ENV,
    },
    StateFile {
        name: DISPATCH_STATE_FILE_NAME,
        env_override: DISPATCH_STATE_ENV,
    },
    StateFile {
        name: FEEDBACK_FILE_NAME,
        env_override: crate::feedback::FEEDBACK_PATH_ENV,
    },
    StateFile {
        name: CATALOG_CACHE_FILE_NAME,
        env_override: crate::catalog::CACHE_PATH_ENV,
    },
    StateFile {
        name: LOG_FILE_NAME,
        env_override: "",
    },
    // The database URL is not a path, so its "override" wins by being a
    // different server entirely: with DATABASE_URL set the SQLite file below
    // is never created.
    StateFile {
        name: DB_FILE_NAME,
        env_override: "DATABASE_URL",
    },
    StateFile {
        name: crate::config::secrets::SECRETS_KEY_FILE_NAME,
        env_override: "",
    },
    StateFile {
        name: USER_CONFIG_FILE_NAME,
        env_override: "",
    },
    StateFile {
        name: REPORTS_DIR_NAME,
        env_override: "",
    },
];

static DATA_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

/// The explicit data dir: the value set by [`set_data_dir`], else a non-empty
/// `REVIEW_DATA_DIR`. `None` means "derive the root the pre-flag way".
pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = DATA_DIR.read().unwrap_or_else(|e| e.into_inner()).clone() {
        return Some(dir);
    }
    std::env::var(DATA_DIR_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Create `path` if missing and make it the process data dir. Returns the
/// absolute root the server will use — absolute because the value ends up in
/// database URLs and log output, where a cwd-relative path is ambiguous.
pub fn set_data_dir(path: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(path).with_context(|| format!("failed to create the data dir {}", path.display()))?;
    let root = std::fs::canonicalize(path).unwrap_or_else(|_| absolutize(path));
    *DATA_DIR.write().unwrap_or_else(|e| e.into_inner()) = Some(root.clone());
    Ok(root)
}

/// Apply `serve --data-dir` (or `REVIEW_DATA_DIR` when the flag is absent),
/// creating the directory. `None` when neither is configured — the caller then
/// keeps the pre-flag defaults.
pub fn apply_data_dir(flag: Option<&Path>) -> anyhow::Result<Option<PathBuf>> {
    let candidate = match flag {
        Some(path) => Some(path.to_path_buf()),
        None => std::env::var(DATA_DIR_ENV)
            .ok()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
    };
    match candidate {
        Some(path) => Ok(Some(set_data_dir(&path)?)),
        None => Ok(None),
    }
}

/// Root that every default state path resolves under:
/// data dir → `REVIEW_ENGINE_CONFIG_DIR` → `~/.config/review-engine`.
/// `None` when none of the three is available (degraded environments run
/// without persistence, exactly as before).
pub fn state_dir() -> Option<PathBuf> {
    state_dir_from(
        data_dir(),
        std::env::var(CONFIG_DIR_ENV).ok().as_deref(),
        home::home_dir(),
    )
}

/// Pure form of [`state_dir`]: the explicit root wins, then the config-dir
/// override, then `<home>/.config/review-engine`.
pub fn state_dir_from(root: Option<PathBuf>, config_dir_env: Option<&str>, home: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(dir) = root {
        return Some(dir);
    }
    if let Some(dir) = config_dir_env.filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    home.map(|dir| dir.join(".config").join("review-engine"))
}

/// `<state_dir>/<file_name>` — the default location of one artifact.
pub fn state_file(file_name: &str) -> Option<PathBuf> {
    state_dir().map(|dir| dir.join(file_name))
}

/// `<state_dir>/<USER_CONFIG_FILE_NAME>` — the user-level config file.
pub fn user_config_path() -> Option<PathBuf> {
    state_file(USER_CONFIG_FILE_NAME)
}

/// Resolve one artifact: a non-empty `env_value` (its full-path override)
/// wins verbatim, otherwise the file lands in the current data dir.
pub fn resolve_artifact(env_value: Option<&str>, file_name: &str) -> Option<PathBuf> {
    resolve_artifact_at(env_value, file_name, state_dir())
}

/// Pure form of [`resolve_artifact`] with an explicit root.
pub fn resolve_artifact_at(env_value: Option<&str>, file_name: &str, root: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(path) = env_value.filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(path));
    }
    root.map(|dir| dir.join(file_name))
}

/// Every per-artifact override that is set right now, as
/// `(env var, artifact name)`. Empty when each artifact resolves under the
/// root — the isolation guarantee holds exactly then.
pub fn escaping_overrides() -> Vec<(&'static str, &'static str)> {
    STATE_FILES
        .iter()
        .filter(|file| !file.env_override.is_empty())
        .filter(|file| std::env::var(file.env_override).is_ok_and(|value| !value.is_empty()))
        .map(|file| (file.env_override, file.name))
        .collect()
}

/// Full-path override for the dispatcher state file.
pub const DISPATCH_STATE_ENV: &str = "REVIEW_DISPATCH_STATE";

fn absolutize(path: &Path) -> PathBuf {
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        Err(_) => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (a) Every default state path sits under the data dir, not under the
    /// default config dir. Asserting the whole table, not one path, is the
    /// point: a half-honoured flag silently writes part of the state into the
    /// real home directory.
    #[test]
    fn every_default_artifact_lands_under_the_data_dir() {
        let root = PathBuf::from("/srv/reng-instance-a");
        let home = Some(PathBuf::from("/home/alice"));
        let resolved_root = state_dir_from(Some(root.clone()), None, home.clone());
        assert_eq!(resolved_root, Some(root.clone()));

        for file in STATE_FILES {
            let path = resolve_artifact_at(None, file.name, resolved_root.clone())
                .unwrap_or_else(|| panic!("{} must resolve under the data dir", file.name));
            assert!(
                path.starts_with(&root),
                "{} must land under {} (env {}), got {}",
                file.name,
                root.display(),
                file.env_override,
                path.display()
            );
            assert_eq!(path, root.join(file.name));
        }

        // The user-level config file is the same computation as `state_file`.
        assert_eq!(
            resolve_artifact_at(None, USER_CONFIG_FILE_NAME, resolved_root),
            Some(root.join(USER_CONFIG_FILE_NAME))
        );
    }

    /// (b) Without the flag the root is today's default: a `HOME`-relative
    /// `~/.config/review-engine` (or `REVIEW_ENGINE_CONFIG_DIR` when set).
    #[test]
    fn without_a_data_dir_the_root_is_the_pre_flag_default() {
        assert_eq!(
            state_dir_from(None, None, Some(PathBuf::from("/home/alice"))),
            Some(PathBuf::from("/home/alice/.config/review-engine"))
        );
        assert_eq!(
            state_dir_from(None, Some("/app/config"), Some(PathBuf::from("/home/alice"))),
            Some(PathBuf::from("/app/config")),
            "REVIEW_ENGINE_CONFIG_DIR must keep winning over the home default"
        );
        assert_eq!(
            state_dir_from(None, Some(""), Some(PathBuf::from("/home/alice"))),
            Some(PathBuf::from("/home/alice/.config/review-engine")),
            "an empty REVIEW_ENGINE_CONFIG_DIR falls back, as before"
        );
        assert_eq!(state_dir_from(None, None, None), None);
        assert_eq!(
            resolve_artifact_at(None, DISPATCH_STATE_FILE_NAME, None),
            None,
            "no root and no env override means no persistence"
        );
    }

    /// The data dir outranks `REVIEW_ENGINE_CONFIG_DIR`: the flag is a request
    /// for *this* instance, the env var is a deployment-wide default.
    #[test]
    fn the_data_dir_outranks_the_config_dir_env() {
        assert_eq!(
            state_dir_from(
                Some(PathBuf::from("/srv/instance-b")),
                Some("/app/config"),
                Some(PathBuf::from("/home/alice"))
            ),
            Some(PathBuf::from("/srv/instance-b"))
        );
    }

    /// (c) An explicitly set per-artifact env var still wins over the root —
    /// the documented escape hatch for pointing one file at a mount.
    #[test]
    fn an_explicit_artifact_env_var_wins_over_the_root() {
        let root = Some(PathBuf::from("/srv/reng-instance-a"));
        for file in STATE_FILES {
            // DATABASE_URL is a URL, not a path: it replaces the database
            // rather than relocating `review.db`.
            if file.env_override.is_empty() || file.env_override == "DATABASE_URL" {
                continue;
            }
            let custom = format!("/mnt/state/{}", file.name);
            assert_eq!(
                resolve_artifact_at(Some(&custom), file.name, root.clone()),
                Some(PathBuf::from(&custom)),
                "{} must win verbatim",
                file.env_override
            );
        }
        assert_eq!(
            resolve_artifact_at(Some(""), AUTH_FILE_NAME, root.clone()),
            root.clone().map(|dir| dir.join(AUTH_FILE_NAME)),
            "an empty value is not an override"
        );
    }

    /// (d) Two instances with different data dirs resolve disjoint paths for
    /// every artifact — no shared DB, state file, key or log.
    #[test]
    fn two_data_dirs_resolve_disjoint_state() {
        let a = state_dir_from(
            Some(PathBuf::from("/srv/instance-a")),
            None,
            Some(PathBuf::from("/home/alice")),
        );
        let b = state_dir_from(
            Some(PathBuf::from("/srv/instance-b")),
            None,
            Some(PathBuf::from("/home/alice")),
        );
        for file in STATE_FILES {
            let pa = resolve_artifact_at(None, file.name, a.clone()).unwrap();
            let pb = resolve_artifact_at(None, file.name, b.clone()).unwrap();
            assert_ne!(pa, pb, "{} must not be shared between instances", file.name);
            assert!(
                !pa.starts_with("/home/alice"),
                "{} must leave the default root alone",
                file.name
            );
        }
    }

    /// The per-artifact resolvers all route through this module: with no data
    /// dir and no override they resolve to `<root>/<name>` for the same root
    /// [`state_file`] reports.
    ///
    /// A resolver whose override is set in this process (a concurrent test
    /// holding an `EnvGuard`, or a developer's shell) is skipped — the
    /// end-to-end check that a real `--data-dir` moves the written files is
    /// `tests/cli/data_dir.rs`.
    #[test]
    fn artifact_resolvers_agree_with_the_shared_layout() {
        let resolved: [(&str, Option<PathBuf>); 7] = [
            (
                UI_STATE_FILE_NAME,
                crate::server::api::config::persist::resolve_ui_state_path(),
            ),
            (AUTH_FILE_NAME, crate::server::auth::default_auth_file_path()),
            (
                DISPATCH_STATE_FILE_NAME,
                crate::server::dispatcher::default_state_path(),
            ),
            (FEEDBACK_FILE_NAME, crate::feedback::default_path()),
            (CATALOG_CACHE_FILE_NAME, crate::catalog::default_cache_path()),
            (LOG_FILE_NAME, crate::server::log_collector::default_ndjson_path()),
            (USER_CONFIG_FILE_NAME, user_config_path()),
        ];
        for (file_name, path) in resolved {
            let overridden = STATE_FILES
                .iter()
                .find(|file| file.name == file_name)
                .is_some_and(|file| {
                    !file.env_override.is_empty()
                        && std::env::var(file.env_override).is_ok_and(|value| !value.is_empty())
                });
            if overridden {
                continue;
            }
            assert_eq!(path, state_file(file_name), "{file_name} must come from the state dir");
        }
        assert_eq!(
            crate::models::default_output_dir(),
            state_file(REPORTS_DIR_NAME)
                .map(|dir| dir.to_string_lossy().into_owned())
                .expect("a state dir must resolve in tests"),
            "the report output dir is part of the state layout"
        );
    }
}
