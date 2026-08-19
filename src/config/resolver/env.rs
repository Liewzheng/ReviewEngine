//! Environment-variable resolution: `CODE_AUDIT_*` overrides and `LLM_CONFIG`.

use std::collections::HashMap;

use crate::models::*;

pub(super) fn apply_env_overrides(mut config: AppConfig) -> AppConfig {
    if let Ok(val) = std::env::var("CODE_AUDIT_COMMANDS") {
        match toml::from_str::<HashMap<String, bool>>(&val) {
            Ok(parsed) => {
                for (k, v) in parsed {
                    config.commands.insert(k, v);
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "CODE_AUDIT_COMMANDS is set but could not be parsed as a boolean map; ignoring"
                );
            }
        }
    }
    if let Ok(val) = std::env::var("CODE_AUDIT_SCORING_ENABLED") {
        config.scoring.enabled = val == "true" || val == "1";
    }
    config
}

/// Parse LLM provider configs from the `LLM_CONFIG` environment variable.
///
/// The variable must contain a JSON array of [`LLMConfig`] objects. Returns
/// an empty vector when the variable is unset, empty, `"[]"`, or fails to
/// parse — in the last case a warning is logged instead of aborting, so
/// callers can fall through to the next source in their precedence chain.
pub fn llm_configs_from_env() -> Vec<LLMConfig> {
    let Ok(json) = std::env::var("LLM_CONFIG") else {
        return Vec::new();
    };
    if json.is_empty() || json == "[]" {
        return Vec::new();
    }
    match serde_json::from_str(&json) {
        Ok(configs) => configs,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "LLM_CONFIG is set but could not be parsed as a JSON array of LLM configs; ignoring"
            );
            Vec::new()
        }
    }
}

/// Fill `config.llm` from the `LLM_CONFIG` environment variable when no
/// `[[llm]]` providers were resolved from config files.
///
/// The environment variable is a fallback only: a non-empty `config.llm`
/// always wins and is never overridden.
pub fn apply_llm_env_fallback(config: &mut AppConfig) {
    if config.llm.is_empty() {
        config.llm = llm_configs_from_env();
    }
}
