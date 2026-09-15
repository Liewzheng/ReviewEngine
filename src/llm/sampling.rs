//! Per-call LLM latency sampling (RENG-57).
//!
//! The LLM Status page needs two numbers that only a call-level history can
//! give: a real average latency over a window, and a sparkline of how a
//! provider has behaved. Everything that existed before was instantaneous —
//! the connectivity probe (RENG-36) measures one `GET /models` round trip —
//! or review-level (`reviews.llm_summary`, RENG-38/56, names the provider and
//! model of a review but carries no timing).
//!
//! This module defines what gets recorded and the seam the recording travels
//! through. [`LlmCallSink`] is implemented by the store layer
//! (`crate::store::llm_samples::StoreLlmCallSink`) and attached to the
//! [`LLMClient`](super::client::LLMClient) of a review; a client with no sink
//! (the CLI, unit tests) records nothing.
//!
//! Invariants:
//!
//! - **A sample is one ATTEMPT, not one logical call.** A config that is
//!   retried three times produces three samples, each with its own `attempt`
//!   number, and a failure that advances the chain keeps its row. That is
//!   deliberate: the retries and the failed fallback attempts are exactly the
//!   latency the provider costs that no other table records.
//! - **Recording never affects the review.** [`LlmCallSink::record`] returns
//!   nothing: an implementation logs its own write failure and gives up. A
//!   missing sample is a gap in a statistic, never a failed review.
//! - **The sink carries the review identity.** A sink is created per review
//!   (bound to its id by the caller), so a sample cannot be attributed to the
//!   wrong review and the client needs to know nothing about reviews at all.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Longest error text kept on a sample. A provider error can carry a whole
/// response body; the column only needs to answer "why did this attempt fail",
/// and rows are pruned by retention rather than trimmed later.
pub const ERROR_MAX_CHARS: usize = 512;

/// One LLM call attempt, as recorded for the LLM Status page.
///
/// `success == false` samples are recorded too — they are what makes the
/// call-level failure data available (RENG-56's `successRate` is review-level
/// and cannot see them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmCallSample {
    /// When the attempt was made (UTC, recorded client-side).
    pub at: DateTime<Utc>,
    /// `LLMConfig.provider` of the config the attempt used — the same name
    /// `reviews.llm_summary` records, so both aggregates agree.
    pub provider: String,
    /// `LLMConfig.model` of the config the attempt used.
    pub model: String,
    /// RENG-75: `LLMConfig::entry_fp` of the config the attempt used — which
    /// CARD served the call when several share a provider name. Server-side
    /// only (it hashes the key): never logged or returned by an API.
    pub entry_fp: String,
    /// Round-trip time of this attempt, in milliseconds.
    pub latency_ms: u64,
    /// `true` when the attempt produced a completion.
    pub success: bool,
    /// The failure, truncated to [`ERROR_MAX_CHARS`]; `None` on success.
    pub error: Option<String>,
    /// 1-based position of this config in the fallback chain the call walked
    /// (`1` = the head, i.e. the primary). A direct
    /// [`LLMClient::complete`](super::client::LLMClient::complete) call has no
    /// chain and is recorded as position 1.
    pub chain_position: u32,
    /// 1-based attempt number within that config (the RENG-35 retry loop).
    pub attempt: u32,
}

impl LlmCallSample {
    /// True when this attempt was answered by a config behind the chain head,
    /// i.e. the primary did not serve it (RENG-55's fallback notion, derived
    /// from the chain position here).
    pub fn is_fallback(&self) -> bool {
        self.chain_position > 1
    }
}

/// Where a review's recorded samples go.
///
/// The single implementation in production writes a row to `llm_call_samples`
/// and logs (never propagates) a write failure; tests substitute an in-memory
/// collector. Implementations must not panic and must not block a review on
/// their own availability.
#[async_trait]
pub trait LlmCallSink: Send + Sync {
    /// Record one attempt. Best-effort by contract: failures are the
    /// implementation's to log, and the caller continues either way.
    async fn record(&self, sample: &LlmCallSample);
}

/// Truncate an error message to [`ERROR_MAX_CHARS`], keeping a marker so a
/// clipped message is recognisable in the table.
pub fn truncate_error(message: &str) -> String {
    if message.chars().count() <= ERROR_MAX_CHARS {
        return message.to_string();
    }
    let mut out: String = message.chars().take(ERROR_MAX_CHARS).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(chain_position: u32) -> LlmCallSample {
        LlmCallSample {
            at: Utc::now(),
            provider: "xiaomi".to_string(),
            model: "mimo".to_string(),
            entry_fp: "fp-xiaomi".to_string(),
            latency_ms: 120,
            success: true,
            error: None,
            chain_position,
            attempt: 1,
        }
    }

    /// A hit behind the chain head is a fallback; the head (and a direct
    /// `complete` call, recorded as position 1) is not.
    #[test]
    fn fallback_is_derived_from_the_chain_position() {
        assert!(!sample(1).is_fallback());
        assert!(sample(2).is_fallback());
        assert!(sample(7).is_fallback());
    }

    /// Errors are bounded (a provider body can be arbitrarily long) and a
    /// clipped one is marked as such; short messages pass through verbatim.
    #[test]
    fn errors_are_truncated_for_the_row() {
        assert_eq!(truncate_error("HTTP 401 Unauthorized"), "HTTP 401 Unauthorized");
        let long = "x".repeat(ERROR_MAX_CHARS + 100);
        let truncated = truncate_error(&long);
        assert_eq!(truncated.chars().count(), ERROR_MAX_CHARS + 1);
        assert!(truncated.ends_with('…'));
        // Multibyte-safe: a char count, not a byte slice.
        let multibyte = "错".repeat(ERROR_MAX_CHARS + 10);
        assert_eq!(truncate_error(&multibyte).chars().count(), ERROR_MAX_CHARS + 1);
    }
}
