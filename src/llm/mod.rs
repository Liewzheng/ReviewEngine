//! LLM client abstraction, provider selection, and rate limiting.
//!
//! The `client` submodule provides [`LLMClient`], which handles HTTP
//! communication with language model APIs (OpenAI, Anthropic, etc.) with
//! automatic fallback across multiple configured endpoints. The `provider`
//! submodule normalises provider-specific details. The `rate_limiter`
//! submodule enforces concurrency and token-bucket limits to avoid
//! overwhelming API endpoints. The helper function [`select_llm_config`]
//! resolves which LLM configuration to use for a given expert.

pub mod client;
pub mod provider;
pub mod rate_limiter;

use crate::models::{ExpertDef, LLMConfig};

pub(crate) fn select_llm_config(expert: &ExpertDef, configs: &[LLMConfig]) -> Vec<LLMConfig> {
    if let Some(first) = configs.first() {
        if !expert.config.model.is_empty() {
            let mut custom = first.clone();
            custom.model = expert.config.model.clone();
            vec![custom]
        } else {
            configs.to_vec()
        }
    } else {
        vec![LLMConfig {
            provider: "default".to_string(),
            model: "gpt-4".to_string(),
            api_key: String::new(),
            api_base: String::new(),
            max_tokens: 4096,
            temperature: 0.3,
        }]
    }
}
