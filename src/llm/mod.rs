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
pub mod probe;
pub mod provider;
pub mod rate_limiter;
pub mod sampling;

use crate::models::{ExpertDef, LLMConfig};

/// The authoritative provider chain for review execution: the **primary**
/// first, then the remaining configs in their stored order (RENG-55), with
/// **disabled entries skipped entirely** (RENG-75).
///
/// `configs` is the stored provider list (Web UI / `llm_providers` array
/// order) and `primary` the persisted `llm.primaryProvider` selection. The
/// rules, nothing else:
///
/// - Disabled entries never join the chain: a review never runs on them and
///   the head is always an ENABLED provider.
/// - `primary` names a stored, enabled provider → that entry moves to the
///   head.
/// - `primary` is empty, names no stored provider, or names a DISABLED one →
///   the stored order of the enabled subset is already authoritative and its
///   head (the first enabled entry) is the effective primary.
///
/// The relative order of the remaining entries never changes, so the chain
/// the runtime walks is exactly the one the UI shows (`chain_positions`).
pub fn ordered_llm_configs(primary: &str, configs: &[LLMConfig]) -> Vec<LLMConfig> {
    chain_order_indices(primary, configs)
        .into_iter()
        .map(|i| configs[i].clone())
        .collect()
}

/// 1-based rank of every entry of `configs` in the chain of
/// [`ordered_llm_configs`] — the primary is `Some(1)`. Index-aligned with
/// `configs`; a DISABLED entry is not in the chain and reports `None`
/// (RENG-75: the API maps it to a `null` `chainPosition`).
pub fn chain_positions(primary: &str, configs: &[LLMConfig]) -> Vec<Option<usize>> {
    let order = chain_order_indices(primary, configs);
    let mut ranks = vec![None; configs.len()];
    for (rank, &index) in order.iter().enumerate() {
        ranks[index] = Some(rank + 1);
    }
    ranks
}

/// Zero-based indices into `configs`, in chain order. The index form of
/// [`ordered_llm_configs`], for callers that must keep their own per-entry
/// metadata (e.g. the `{provider}-{index}` ids of `GET /llm/providers`).
/// Disabled entries are excluded here, which is what keeps them out of every
/// consumer of the chain (RENG-75).
fn chain_order_indices(primary: &str, configs: &[LLMConfig]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..configs.len()).filter(|&i| !configs[i].disabled).collect();
    if primary.is_empty() {
        return order;
    }
    if let Some(pos) = configs.iter().position(|c| !c.disabled && c.provider == primary) {
        order.retain(|&i| i != pos);
        order.insert(0, pos);
    }
    order
}

/// Resolve the LLM chain for one expert.
///
/// `configs` MUST already be the authoritative chain ([`ordered_llm_configs`],
/// i.e. enabled entries only) — its HEAD is the primary provider, so an
/// expert with a custom `model` gets the **primary** config as its base with
/// that model substituted. Before RENG-55 this cloned `configs.first()` of
/// the raw stored list, which silently ignored the user's primary selection
/// whenever the primary was not the first stored entry.
///
/// An expert with a custom model runs against that single provider (no
/// fallback: the custom model need not exist on the other providers);
/// otherwise the whole chain is passed on for [`crate::llm::client::LLMClient::complete_with_fallback`].
pub(crate) fn select_llm_config(expert: &ExpertDef, configs: &[LLMConfig]) -> Vec<LLMConfig> {
    let Some(primary) = configs.first() else {
        return vec![LLMConfig {
            provider: "default".to_string(),
            model: "gpt-4".to_string(),
            api_key: String::new(),
            api_base: String::new(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
            disabled: false,
        }];
    };
    if expert.config.model.is_empty() {
        configs.to_vec()
    } else {
        let mut custom = primary.clone();
        custom.model = expert.config.model.clone();
        vec![custom]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ExpertTomlDef;

    fn cfg(provider: &str, model: &str) -> LLMConfig {
        LLMConfig {
            provider: provider.to_string(),
            model: model.to_string(),
            api_key: "k".to_string(),
            api_base: format!("https://api.{provider}.example/v1"),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
            disabled: false,
        }
    }

    fn expert_with_model(name: &str, model: &str) -> ExpertDef {
        ExpertDef {
            name: name.to_string(),
            trigger: crate::models::ExpertTrigger::Always,
            prompt: String::new(),
            config: ExpertTomlDef {
                model: model.to_string(),
                ..Default::default()
            },
        }
    }

    /// The production symptom (RENG-55): stored order [xiaomi, deepseek] with
    /// `deepseek` selected as primary must run deepseek first.
    #[test]
    fn chain_moves_primary_to_the_head() {
        let stored = vec![cfg("xiaomi", "mimo-v2.5"), cfg("deepseek", "deepseek-v4")];
        let chain = ordered_llm_configs("deepseek", &stored);
        assert_eq!(
            chain.iter().map(|c| c.provider.as_str()).collect::<Vec<_>>(),
            vec!["deepseek", "xiaomi"]
        );
        // The stored list itself is never mutated.
        assert_eq!(stored[0].provider, "xiaomi");
    }

    /// Every remaining entry keeps its stored relative order — the fallback
    /// sequence the user sees is the one the runtime walks.
    #[test]
    fn chain_keeps_the_stored_order_of_the_remaining_entries() {
        let stored = vec![cfg("a", "m"), cfg("b", "m"), cfg("c", "m"), cfg("d", "m")];
        let chain = ordered_llm_configs("c", &stored);
        assert_eq!(
            chain.iter().map(|c| c.provider.as_str()).collect::<Vec<_>>(),
            vec!["c", "a", "b", "d"]
        );
        // A primary already at the head is a no-op.
        let chain = ordered_llm_configs("a", &stored);
        assert_eq!(
            chain.iter().map(|c| c.provider.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c", "d"]
        );
    }

    /// An empty or unknown primary falls back to the stored order: its head is
    /// the effective primary (`GET /config` can echo a provider the runtime no
    /// longer holds, e.g. after a delete from another tab).
    #[test]
    fn chain_falls_back_to_stored_order_for_empty_or_unknown_primary() {
        let stored = vec![cfg("a", "m"), cfg("b", "m")];
        for primary in ["", "ghost"] {
            let chain = ordered_llm_configs(primary, &stored);
            assert_eq!(
                chain.iter().map(|c| c.provider.as_str()).collect::<Vec<_>>(),
                vec!["a", "b"],
                "primary {primary:?} must keep the stored order"
            );
        }
        assert!(ordered_llm_configs("a", &[]).is_empty());
    }

    /// `chain_positions` is what the LLM page renders ("链序 #N"): 1-based,
    /// index-aligned with the stored list; a disabled entry has no rank.
    #[test]
    fn chain_positions_are_one_based_and_index_aligned() {
        let stored = vec![cfg("a", "m"), cfg("b", "m"), cfg("c", "m")];
        assert_eq!(chain_positions("c", &stored), vec![Some(2), Some(3), Some(1)]);
        assert_eq!(chain_positions("ghost", &stored), vec![Some(1), Some(2), Some(3)]);
        assert!(chain_positions("a", &[]).is_empty());
    }

    /// RENG-75: disabled entries are skipped ENTIRELY — the chain numbers
    /// only the enabled subset and the stored order is the priority.
    /// `[enabled, disabled, enabled]` → a chain of 2, positions 1 and 2.
    #[test]
    fn chain_skips_disabled_entries() {
        let mut stored = vec![cfg("a", "m"), cfg("b", "m"), cfg("c", "m")];
        stored[1].disabled = true;

        let chain = ordered_llm_configs("", &stored);
        assert_eq!(
            chain.iter().map(|c| c.provider.as_str()).collect::<Vec<_>>(),
            vec!["a", "c"],
            "the disabled middle entry never joins the chain"
        );
        assert_eq!(
            chain_positions("", &stored),
            vec![Some(1), None, Some(2)],
            "a disabled entry has no chain position"
        );
        // A primary selection naming the DISABLED entry cannot resurrect it:
        // the first enabled entry stays the head.
        let chain = ordered_llm_configs("b", &stored);
        assert_eq!(
            chain.iter().map(|c| c.provider.as_str()).collect::<Vec<_>>(),
            vec!["a", "c"],
            "a disabled recorded primary is skipped like any disabled entry"
        );
    }

    /// RENG-75: disabling the chain head moves the head to the next enabled
    /// entry — the recorded primary is normalised away, not honoured.
    #[test]
    fn disabling_the_head_moves_the_head_to_the_next_enabled() {
        let mut stored = vec![cfg("a", "m"), cfg("b", "m"), cfg("c", "m")];
        // "a" is the recorded primary (and the head).
        assert_eq!(ordered_llm_configs("a", &stored)[0].provider, "a");
        stored[0].disabled = true;

        let chain = ordered_llm_configs("a", &stored);
        assert_eq!(
            chain.iter().map(|c| c.provider.as_str()).collect::<Vec<_>>(),
            vec!["b", "c"],
            "the next enabled entry becomes the head"
        );
        assert_eq!(chain_positions("a", &stored), vec![None, Some(1), Some(2)]);
    }

    /// RENG-75: every provider disabled → an empty chain (the review path
    /// turns this into the fast, named all-disabled failure instead of
    /// running experts against the placeholder below).
    #[test]
    fn chain_is_empty_when_every_provider_is_disabled() {
        let mut stored = vec![cfg("a", "m"), cfg("b", "m")];
        for c in &mut stored {
            c.disabled = true;
        }
        assert!(ordered_llm_configs("a", &stored).is_empty());
        assert_eq!(chain_positions("a", &stored), vec![None, None]);
    }

    /// The custom-model branch must take the PRIMARY as its base — the fixed
    /// half of RENG-55. The chain passed here is head-first, so the resulting
    /// expert runs on deepseek (the primary) instead of xiaomi (first stored).
    #[test]
    fn select_llm_config_uses_the_primary_as_base_for_a_custom_expert_model() {
        let stored = vec![cfg("xiaomi", "mimo-v2.5"), cfg("deepseek", "deepseek-v4")];
        let chain = ordered_llm_configs("deepseek", &stored);
        let expert = expert_with_model("security", "deepseek-reasoner");

        let selected = select_llm_config(&expert, &chain);
        assert_eq!(selected.len(), 1, "a custom expert model runs one provider");
        assert_eq!(selected[0].provider, "deepseek", "primary must be the base");
        assert_eq!(selected[0].model, "deepseek-reasoner");
        // Everything else comes from the primary's entry.
        assert_eq!(selected[0].api_base, chain[0].api_base);
    }

    /// No custom model → the whole chain is passed through unchanged (the
    /// fallback order is preserved).
    #[test]
    fn select_llm_config_passes_the_whole_chain_without_a_custom_model() {
        let stored = vec![cfg("a", "m"), cfg("b", "m")];
        let chain = ordered_llm_configs("b", &stored);
        let selected = select_llm_config(&expert_with_model("plain", ""), &chain);
        assert_eq!(
            selected.iter().map(|c| c.provider.as_str()).collect::<Vec<_>>(),
            vec!["b", "a"]
        );
    }

    /// Empty config list keeps the legacy placeholder so the caller fails with
    /// "no api_base set" instead of panicking.
    #[test]
    fn select_llm_config_placeholder_without_configs() {
        let selected = select_llm_config(&expert_with_model("plain", ""), &[]);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].provider, "default");
    }
}
