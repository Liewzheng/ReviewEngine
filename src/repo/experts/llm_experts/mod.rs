//! LLM-powered repo-level experts for architecture and code quality analysis.

mod scoring;
mod architecture;
mod code_quality;
#[cfg(test)]
mod tests;

pub use architecture::ArchitectureLead;
pub use code_quality::CodeQuality;

pub(crate) use scoring::{
    append_facts_block, call_scoring, median_score, parse_expert_yaml, scoring_configs,
    scoring_sample_count, truncate_excerpt, ARCHITECTURE_LEAD_KEYS, CODE_QUALITY_KEYS,
    SCORING_TEMPERATURE, SCORING_TEMPERATURE_MAX, EXCERPT_MAX_BYTES,
};
pub(crate) use architecture::architecture_user_prompt;
pub(crate) use code_quality::{code_quality_user_prompt, render_code_quality_system};
