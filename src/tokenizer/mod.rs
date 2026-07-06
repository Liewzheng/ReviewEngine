//! Token counting utilities for LLM context management.
//!
//! This module is part of the review-engine CodeReview Board platform.
//!
//! Uses `tiktoken-rs` BPE tokenizers to count tokens in text strings
//! for various model families (GPT-4o, GPT-4, GPT-3.5, DeepSeek,
//! Claude). The [`count_tokens`] function selects the correct encoding
//! based on model name and returns the token count. [`truncate_to_tokens`]
//! truncates text to a maximum token budget. This is critical for
//! staying within model context windows and estimating API costs.

use anyhow::Result;
use std::sync::OnceLock;
use tiktoken_rs::{cl100k_base, o200k_base, CoreBPE};

/// Map model name prefix to encoding name.
fn encoding_for_model(model: &str) -> &'static str {
    if model.starts_with("gpt-4o") {
        "o200k_base"
    } else if model.starts_with("gpt-4") || model.starts_with("gpt-3.5") {
        "cl100k_base"
    } else if model.starts_with("deepseek") {
        "cl100k_base"
    } else if model.starts_with("claude") {
        "cl100k_base"
    } else {
        "cl100k_base"
    }
}

fn get_bpe(encoding: &str) -> Result<&'static CoreBPE> {
    static O200K: OnceLock<CoreBPE> = OnceLock::new();
    static CL100K: OnceLock<CoreBPE> = OnceLock::new();

    match encoding {
        "o200k_base" => {
            if O200K.get().is_none() {
                let bpe = o200k_base().map_err(|e| anyhow::anyhow!("failed to load o200k_base: {}", e))?;
                O200K.set(bpe).ok();
            }
            O200K.get().ok_or_else(|| anyhow::anyhow!("failed to load o200k_base"))
        }
        _ => {
            if CL100K.get().is_none() {
                let bpe = cl100k_base().map_err(|e| anyhow::anyhow!("failed to load cl100k_base: {}", e))?;
                CL100K.set(bpe).ok();
            }
            CL100K
                .get()
                .ok_or_else(|| anyhow::anyhow!("failed to load cl100k_base"))
        }
    }
}

/// Count the number of tokens in a text string for a given model.
///
/// Uses BPE tokenization via `tiktoken-rs`. Falls back to `cl100k_base`
/// for unknown models.
///
/// # Examples
///
/// ```
/// use review_engine::tokenizer::count_tokens;
/// let tokens = count_tokens("Hello, world!", "gpt-4").unwrap();
/// assert!(tokens > 0);
/// ```
pub fn count_tokens(text: &str, model: &str) -> Result<usize> {
    let encoding = encoding_for_model(model);
    match get_bpe(encoding) {
        Ok(bpe) => {
            let tokens = bpe.encode_with_special_tokens(text);
            Ok(tokens.len())
        }
        Err(e) => {
            tracing::warn!(
                "Tokenizer load failed for model '{}': {}. Falling back to whitespace word count.",
                model,
                e
            );
            Ok(text.split_whitespace().count())
        }
    }
}

/// Count tokens using a specific encoding name directly (e.g. `"cl100k_base"`, `"o200k_base"`).
///
/// Bypasses model-name detection; useful when the encoding is known
/// ahead of time or for non-standard model mappings.
pub fn count_tokens_with_encoding(text: &str, encoding: &str) -> Result<usize> {
    match get_bpe(encoding) {
        Ok(bpe) => {
            let tokens = bpe.encode_with_special_tokens(text);
            Ok(tokens.len())
        }
        Err(e) => {
            tracing::warn!(
                "Tokenizer load failed for encoding '{}': {}. Falling back to whitespace word count.",
                encoding,
                e
            );
            Ok(text.split_whitespace().count())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_tokens_gpt4() {
        let tokens = count_tokens("Hello, world!", "gpt-4").unwrap();
        assert!(tokens > 0);
        assert!(tokens < 10);
    }

    #[test]
    fn test_count_tokens_gpt4o() {
        let tokens = count_tokens("Hello, world!", "gpt-4o").unwrap();
        assert!(tokens > 0);
    }

    #[test]
    fn test_count_tokens_deepseek() {
        let tokens = count_tokens("Hello, world!", "deepseek-chat").unwrap();
        assert!(tokens > 0);
    }

    #[test]
    fn test_count_tokens_claude() {
        let tokens = count_tokens("Hello, world!", "claude-3-5-sonnet-20241022").unwrap();
        assert!(tokens > 0);
    }

    #[test]
    fn test_count_tokens_empty_string() {
        let tokens = count_tokens("", "gpt-4").unwrap();
        assert_eq!(tokens, 0);
    }

    #[test]
    fn test_count_tokens_long_text() {
        let long = "word ".repeat(1000);
        let tokens = count_tokens(&long, "gpt-4").unwrap();
        // ~0.75 tokens per word on average for English
        assert!(tokens > 500);
        assert!(tokens < 2000);
    }

    #[test]
    fn test_count_tokens_unknown_model_fallback() {
        let tokens = count_tokens("test", "unknown-model").unwrap();
        assert!(tokens > 0);
    }

    #[test]
    fn test_encoding_for_model() {
        assert_eq!(encoding_for_model("gpt-4o"), "o200k_base");
        assert_eq!(encoding_for_model("gpt-4-turbo"), "cl100k_base");
        assert_eq!(encoding_for_model("gpt-3.5-turbo"), "cl100k_base");
        assert_eq!(encoding_for_model("deepseek-chat"), "cl100k_base");
        assert_eq!(encoding_for_model("claude-3-opus"), "cl100k_base");
    }
}
