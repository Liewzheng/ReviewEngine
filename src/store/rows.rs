//! Row structure ⇄ domain structure codecs for the configuration domain.
//!
//! This module is the `enc:` encryption boundary (design/persistence.md
//! §4.1): domain values are live plaintext, row values are the at-rest form.
//! Encrypted at rest: `git_platforms.token / webhook_secret /
//! webhook_signing_secret`, `llm_providers.api_key` (newly inside the
//! boundary — 0.9 stored it plaintext), and each field of the legacy
//! `gitlab` settings JSON. Empty strings stay empty (never encrypted).
//! Values read back WITHOUT the `enc:` prefix are legacy plaintext and pass
//! through unchanged (`decrypt_secret`'s existing semantics).

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::config::secrets::{decrypt_secret, encrypt_secret};
use crate::models::{GitPlatformConfig, LLMConfig};
use crate::server::api::config::persist::PersistedGitlabConfig;

/// At-rest form of one `git_platforms` row.
#[derive(Debug)]
pub(crate) struct GitPlatformRow {
    pub id: String,
    pub name: String,
    pub platform_type: String,
    pub base_url: String,
    pub internal_base_url: String,
    pub token: String,
    pub webhook_secret: String,
    pub webhook_signing_secret: String,
    pub enabled: bool,
    /// JSON fallback bag for non-columnized fields (`allowed_projects`).
    pub raw: String,
    pub updated_at: String,
}

/// At-rest form of one `llm_providers` row.
#[derive(Debug)]
pub(crate) struct LlmProviderRow {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub api_base: String,
    pub api_key: String,
    pub max_tokens: i64,
    pub temperature: f64,
    /// JSON fallback bag: `disable_thinking`, plus `position` — the list
    /// index, because provider order is semantically meaningful (first entry
    /// is the fallback primary) and the table has no sequence column.
    pub raw: String,
    pub updated_at: String,
}

fn encrypt_non_empty(value: &str, key: &[u8; 32]) -> Result<String> {
    if value.is_empty() {
        Ok(String::new())
    } else {
        encrypt_secret(value, key)
    }
}

pub(crate) fn git_platform_to_row(
    platform: &GitPlatformConfig,
    id: String,
    updated_at: String,
    key: &[u8; 32],
) -> Result<GitPlatformRow> {
    // `enabled` has no domain counterpart yet (GitPlatformConfig carries no
    // such field); the column is future-proofing and always written TRUE.
    let raw = if platform.allowed_projects.is_empty() {
        json!({})
    } else {
        json!({ "allowed_projects": platform.allowed_projects })
    };
    Ok(GitPlatformRow {
        id,
        name: platform.name.clone(),
        platform_type: platform.platform_type.clone(),
        base_url: platform.base_url.clone(),
        internal_base_url: platform.internal_base_url.clone(),
        token: encrypt_non_empty(&platform.token, key)?,
        webhook_secret: encrypt_non_empty(&platform.webhook_secret, key)?,
        webhook_signing_secret: encrypt_non_empty(&platform.webhook_signing_secret, key)?,
        enabled: true,
        raw: raw.to_string(),
        updated_at,
    })
}

pub(crate) fn git_platform_from_row(row: GitPlatformRow, key: &[u8; 32]) -> Result<GitPlatformConfig> {
    let raw: Value = serde_json::from_str(&row.raw)
        .with_context(|| format!("git_platforms row {:?} has invalid raw JSON", row.name))?;
    let allowed_projects = raw
        .get("allowed_projects")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Ok(GitPlatformConfig {
        name: row.name,
        platform_type: row.platform_type,
        base_url: row.base_url,
        internal_base_url: row.internal_base_url,
        token: decrypt_secret(&row.token, key)?,
        webhook_secret: decrypt_secret(&row.webhook_secret, key)?,
        webhook_signing_secret: decrypt_secret(&row.webhook_signing_secret, key)?,
        allowed_projects,
    })
}

pub(crate) fn llm_to_row(
    config: &LLMConfig,
    position: usize,
    id: String,
    updated_at: String,
    key: &[u8; 32],
) -> Result<LlmProviderRow> {
    let mut raw = json!({ "position": position as i64 });
    if let Some(disable_thinking) = config.disable_thinking {
        raw["disable_thinking"] = json!(disable_thinking);
    }
    Ok(LlmProviderRow {
        id,
        provider: config.provider.clone(),
        model: config.model.clone(),
        api_base: config.api_base.clone(),
        api_key: encrypt_non_empty(&config.api_key, key)?,
        max_tokens: i64::from(config.max_tokens),
        temperature: f64::from(config.temperature),
        raw: raw.to_string(),
        updated_at,
    })
}

/// List position recorded by [`llm_to_row`]; `None` for rows written by
/// other means (sorts after positioned rows).
pub(crate) fn llm_row_position(row: &LlmProviderRow) -> Option<i64> {
    serde_json::from_str::<Value>(&row.raw).ok()?.get("position")?.as_i64()
}

pub(crate) fn llm_from_row(row: LlmProviderRow, key: &[u8; 32]) -> Result<LLMConfig> {
    let raw: Value = serde_json::from_str(&row.raw)
        .with_context(|| format!("llm_providers row {:?} has invalid raw JSON", row.provider))?;
    let disable_thinking = raw.get("disable_thinking").and_then(Value::as_bool);
    Ok(LLMConfig {
        provider: row.provider,
        model: row.model,
        api_key: decrypt_secret(&row.api_key, key)?,
        api_base: row.api_base,
        max_tokens: u32::try_from(row.max_tokens)
            .with_context(|| format!("llm_providers.max_tokens out of range: {}", row.max_tokens))?,
        temperature: row.temperature as f32,
        disable_thinking,
    })
}

/// Legacy GitLab credentials ⇄ the `app_settings` row at key `gitlab`.
/// Each field is individually `enc:`-encrypted inside the JSON (§3.2 note).
pub(crate) fn legacy_gitlab_to_value(gitlab: &PersistedGitlabConfig, key: &[u8; 32]) -> Result<Value> {
    Ok(json!({
        "token": encrypt_non_empty(&gitlab.token, key)?,
        "webhook_secret": encrypt_non_empty(&gitlab.webhook_secret, key)?,
        "webhook_signing_secret": encrypt_non_empty(&gitlab.webhook_signing_secret, key)?,
    }))
}

pub(crate) fn legacy_gitlab_from_value(value: &Value, key: &[u8; 32]) -> Result<PersistedGitlabConfig> {
    let field = |name: &str| -> Result<String> {
        match value.get(name).and_then(Value::as_str) {
            Some(s) => decrypt_secret(s, key),
            None => Ok(String::new()),
        }
    };
    Ok(PersistedGitlabConfig {
        token: field("token")?,
        webhook_secret: field("webhook_secret")?,
        webhook_signing_secret: field("webhook_signing_secret")?,
    })
}
