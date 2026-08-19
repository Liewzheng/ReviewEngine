//! User-level TOML fallbacks from `~/.config/review-engine/.code-audit-config.toml`.

use serde::Deserialize;

use super::take_llm;
use crate::models::*;

/// Load a valid `[[llm]]` array from the user-level config file at
/// `~/.config/review-engine/.code-audit-config.toml`.
///
/// Returns an empty vector if the file is missing, cannot be parsed, or does not
/// contain a valid non-empty `[[llm]]` array.
pub(super) fn load_user_llm_fallback() -> Vec<LLMConfig> {
    let Some(user_path) =
        home::home_dir().map(|p| p.join(".config").join("review-engine").join(".code-audit-config.toml"))
    else {
        return Vec::new();
    };

    if !user_path.exists() {
        return Vec::new();
    }

    match std::fs::read_to_string(&user_path) {
        Ok(content) => match toml::from_str::<toml::Value>(&content) {
            Ok(val) => {
                if let Some(obj) = val.as_table() {
                    if let Some(llm) = obj.get("llm") {
                        let parsed = take_llm(llm);
                        if !parsed.is_empty() {
                            return parsed;
                        }
                    }
                }
                Vec::new()
            }
            Err(e) => {
                tracing::warn!(
                    path = %user_path.display(),
                    error = %e,
                    "Failed to parse user-level config file as TOML; ignoring LLM fallback"
                );
                Vec::new()
            }
        },
        Err(e) => {
            tracing::warn!(
                path = %user_path.display(),
                error = %e,
                "Failed to read user-level config file; ignoring LLM fallback"
            );
            Vec::new()
        }
    }
}

/// Load the `[report]` section from the user-level config file at
/// `~/.config/review-engine/.code-audit-config.toml`.
///
/// Returns `None` — keeping the built-in defaults — if the file is missing,
/// cannot be read or parsed, or does not contain a valid `[report]` section.
pub(super) fn load_user_report_fallback() -> Option<ReportConfig> {
    let user_path = home::home_dir()?
        .join(".config")
        .join("review-engine")
        .join(".code-audit-config.toml");

    if !user_path.exists() {
        return None;
    }

    match std::fs::read_to_string(&user_path) {
        Ok(content) => match toml::from_str::<toml::Value>(&content) {
            Ok(val) => {
                let report = val.as_table().and_then(|obj| obj.get("report"))?;
                match ReportConfig::deserialize(report.clone()) {
                    Ok(parsed) => Some(parsed),
                    Err(e) => {
                        tracing::warn!(
                            path = %user_path.display(),
                            error = %e,
                            "Failed to parse user-level [report] section; ignoring"
                        );
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    path = %user_path.display(),
                    error = %e,
                    "Failed to parse user-level config file as TOML; ignoring [report] fallback"
                );
                None
            }
        },
        Err(e) => {
            tracing::warn!(
                path = %user_path.display(),
                error = %e,
                "Failed to read user-level config file; ignoring [report] fallback"
            );
            None
        }
    }
}
