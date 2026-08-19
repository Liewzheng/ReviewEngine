//! Configuration resolution, environment overrides, and validation.
//!
//! Provides [`resolve_config`] for auto-detecting and loading configuration
//! from files or inline sources, [`apply_env_overrides`] for environment
//! variable overrides, and [`validate_experts`] for expert weight validation.

mod env;
mod resolve;
#[cfg(test)]
mod tests;
mod user_fallback;

pub use env::{apply_llm_env_fallback, llm_configs_from_env};
pub use resolve::{resolve_config, ConfigResolver};

use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;

use self::env::apply_env_overrides;
use crate::config::defaults::{merge_default, parse_toml};
use crate::models::*;

/// Parse, merge, apply environment overrides, and validate a TOML config string.
pub fn load_and_apply(toml_content: &str) -> Result<AppConfig> {
    let parsed = parse_toml(toml_content)?;
    let merged = merge_default(parsed)?;
    let config = apply_env_overrides(merged);
    validate_experts(&config)?;
    Ok(config)
}

/// Validate that all enabled experts' weights sum to 100.
pub(crate) fn validate_experts(config: &AppConfig) -> Result<()> {
    let total_weight: u16 = config
        .review_experts
        .iter()
        .filter(|(_, e)| e.enabled)
        .map(|(_, e)| e.weight as u16)
        .sum();

    if total_weight == 0 {
        return Ok(()); // no enabled experts
    }

    if total_weight != 100 {
        let details: Vec<String> = config
            .review_experts
            .iter()
            .filter(|(_, e)| e.enabled)
            .map(|(n, e)| format!("{}({})", n, e.weight))
            .collect();
        anyhow::bail!(
            "Enabled experts' weights sum to {}, but must sum to 100. Experts: [{}]",
            total_weight,
            details.join(", "),
        );
    }

    Ok(())
}

/// Extract LLM config array from a parsed TOML value.
fn take_llm(val: &toml::Value) -> Vec<crate::models::LLMConfig> {
    match Vec::<crate::models::LLMConfig>::deserialize(val.clone()) {
        Ok(llm) => llm,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to parse [[llm]] array from TOML; using empty LLM config");
            Vec::new()
        }
    }
}

/// Extract boolean commands map from a parsed TOML value.
fn take_commands(val: &toml::Value) -> HashMap<String, bool> {
    match val.as_table() {
        Some(table) => table
            .iter()
            .filter_map(|(k, v)| v.as_bool().map(|b| (k.clone(), b)))
            .collect(),
        None => {
            tracing::warn!("commands value is not a TOML table; ignoring");
            HashMap::new()
        }
    }
}
