//! Project-level resolution and the user/project precedence chain.

use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Mutex;

use super::env::apply_env_overrides;
use super::user_fallback::{load_user_llm_fallback, load_user_report_fallback};
use super::{load_and_apply, take_commands, take_llm};
use crate::config::defaults::default_config;
use crate::models::*;

#[cfg(test)]
pub(super) static FALLBACK_WARNINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Print a warning to stderr when falling back to the user-level `[[llm]]`
/// configuration because the project-level `[[llm]]` is missing or invalid.
fn print_llm_fallback_warning(path: &std::path::Path, reason: &str) {
    let msg = format!(
        "Warning: project-level [[llm]] in '{}' is {}; using [[llm]] from ~/.config/review-engine/.code-audit-config.toml as fallback.",
        path.display(),
        reason
    );
    eprintln!("{}", msg);
    #[cfg(test)]
    {
        let mut guard = FALLBACK_WARNINGS.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(msg);
    }
}

/// Parse a config file, separating the `llm` section from everything else.
///
/// This lets us treat an invalid project-level `llm` (e.g. `[llm]` instead of
/// `[[llm]]`) as missing, falling back to the user-level config without failing
/// the whole project config.
fn load_config_without_llm(content: &str) -> Result<(AppConfig, Option<toml::Value>)> {
    let val = toml::from_str::<toml::Value>(content)?;
    let mut cleaned = val.clone();
    let raw_project_llm = if let Some(obj) = cleaned.as_table_mut() {
        obj.remove("llm")
    } else {
        None
    };
    let toml_without_llm = toml::to_string(&cleaned)?;
    let config = load_and_apply(&toml_without_llm)?;
    Ok((config, raw_project_llm))
}

/// Resolve the application configuration from the given source (or auto-detect).
///
/// Resolution order:
/// 1. Built-in defaults + environment-variable overrides (base).
/// 2. `~/.config/review-engine/.code-audit-config.toml` — provides a global
///    `[[llm]]` fallback and global `[report]` defaults.
/// 3. `.code-audit-config.toml` in the current directory (or the file specified
///    by `--config`) — overrides the base. Its `[[llm]]` is only used if it
///    parses successfully and is non-empty; otherwise the user-level `[[llm]]`
///    fallback is used. Its `[report]`, if present, replaces the resolved
///    report config wholesale (omitted fields use serde defaults, not the
///    user-level values).
pub async fn resolve_config(source: Option<ConfigSource>) -> Result<AppConfig> {
    match source {
        Some(ConfigSource::Inline(toml_str)) => load_and_apply(&toml_str),
        Some(ConfigSource::Path(path)) => {
            if !std::path::Path::new(&path).exists() {
                anyhow::bail!("config file not found: {}", path);
            }
            let content = tokio::fs::read_to_string(&path).await?;
            let (mut config, raw_project_llm) = load_config_without_llm(&content)?;
            let project_path = std::path::Path::new(&path);
            let project_llm = raw_project_llm.as_ref().map(take_llm).unwrap_or_default();
            config.llm = if !project_llm.is_empty() {
                project_llm
            } else {
                let user_llm = load_user_llm_fallback();
                if !user_llm.is_empty() {
                    let reason = if raw_project_llm.is_none() {
                        "missing"
                    } else {
                        "invalid"
                    };
                    print_llm_fallback_warning(project_path, reason);
                }
                user_llm
            };
            Ok(config)
        }
        None => {
            let default_path = ".code-audit-config.toml";
            let mut config = apply_env_overrides(default_config()?);

            // User-level config provides a global LLM fallback.
            config.llm = load_user_llm_fallback();

            // User-level [report] provides global report defaults; a
            // project-level [report] (handled below) replaces it wholesale.
            if let Some(report) = load_user_report_fallback() {
                config.report = report;
            }

            // Project-level config overrides
            if std::path::Path::new(default_path).exists() {
                match tokio::fs::read_to_string(default_path).await {
                    Ok(content) => {
                        match toml::from_str::<toml::Value>(&content) {
                            Ok(val) => {
                                if let Some(obj) = val.as_table() {
                                    // LLM: override only if project provides valid [[llm]]
                                    match obj.get("llm") {
                                        None => {
                                            if !config.llm.is_empty() {
                                                print_llm_fallback_warning(
                                                    std::path::Path::new(default_path),
                                                    "missing",
                                                );
                                            }
                                        }
                                        Some(llm) => {
                                            let parsed = take_llm(llm);
                                            if !parsed.is_empty() {
                                                config.llm = parsed;
                                            } else if !config.llm.is_empty() {
                                                print_llm_fallback_warning(
                                                    std::path::Path::new(default_path),
                                                    "invalid",
                                                );
                                            }
                                        }
                                    }
                                    // Commands: override
                                    if let Some(cmds) = obj.get("commands") {
                                        config.commands.extend(take_commands(cmds));
                                    }
                                    // Experts: override (project wins over user)
                                    if let Some(review_experts) = obj.get("review_experts") {
                                        match toml::from_str::<HashMap<String, crate::models::ExpertTomlDef>>(
                                            &review_experts.to_string(),
                                        ) {
                                            Ok(parsed) => {
                                                config.review_experts.extend(parsed);
                                            }
                                            Err(e) => {
                                                tracing::warn!(
                                                    path = default_path,
                                                    error = %e,
                                                    "Failed to parse project-level review_experts section; ignoring"
                                                );
                                            }
                                        }
                                    }
                                    // Report: wholesale replacement (project wins over user).
                                    // NOTE: unlike `commands`/`review_experts`, which extend the
                                    // existing map, a present `[report]` replaces `config.report`
                                    // entirely — fields omitted here fall back to the serde
                                    // defaults of `ReportConfig`, NOT to user-level values.
                                    if let Some(report) = obj.get("report") {
                                        match ReportConfig::deserialize(report.clone()) {
                                            Ok(parsed) => {
                                                config.report = parsed;
                                            }
                                            Err(e) => {
                                                tracing::warn!(
                                                    path = default_path,
                                                    error = %e,
                                                    "Failed to parse project-level [report] section; ignoring"
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    path = default_path,
                                    error = %e,
                                    "Failed to parse project-level config file as TOML; ignoring"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = default_path,
                            error = %e,
                            "Failed to read project-level config file; ignoring"
                        );
                    }
                }
            }

            Ok(config)
        }
    }
}

/// A config resolver that wraps [`resolve_config`] for dependency injection.
pub struct ConfigResolver;

impl ConfigResolver {
    /// Create a new `ConfigResolver`.
    pub fn new() -> Self {
        Self
    }

    /// Resolve the application configuration from the given source.
    ///
    /// Delegates to [`resolve_config`]; see its documentation for resolution order.
    pub async fn resolve(&self, source: Option<ConfigSource>) -> Result<AppConfig> {
        resolve_config(source).await
    }
}
