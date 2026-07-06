//! Default configuration values and parsing utilities.
//!
//! Provides the built-in default configuration loaded from
//! `docs/code-audit-default.toml`, TOML parsing, and merge
//! logic that fills in missing fields from user-provided configs.

use crate::models::*;
use anyhow::Result;

const DEFAULT_TOML: &str = include_str!("../../docs/code-audit-default.toml");

/// Load and parse the built-in default configuration from `docs/code-audit-default.toml`.
pub fn default_config() -> Result<AppConfig> {
    Ok(toml::from_str(DEFAULT_TOML)?)
}

/// Load the embedded default configuration (same as [`default_config`]).
#[allow(dead_code)]
pub(crate) fn load_embedded_default() -> Result<AppConfig> {
    default_config()
}

/// Maximum TOML content size in bytes (1 MiB) to prevent memory DoS from oversized configs.
const MAX_TOML_SIZE: usize = 1024 * 1024;

/// Parse a TOML string into an [`AppConfig`].
///
/// Does *not* merge with defaults — call [`merge_default`] afterwards
/// to fill in missing fields.
/// Rejects input larger than 1 MiB to prevent memory exhaustion.
pub fn parse_toml(content: &str) -> Result<AppConfig> {
    if content.len() > MAX_TOML_SIZE {
        anyhow::bail!("TOML config exceeds maximum size of {} bytes", MAX_TOML_SIZE);
    }
    Ok(toml::from_str(content)?)
}

/// Merge a user-supplied [`AppConfig`] with the built-in defaults.
///
/// Fields missing from the user config fall back to default values.
/// Expert and command maps are extended rather than replaced; LLM
/// configs and top-level scalars use the user value when non-empty.
pub fn merge_default(user: AppConfig) -> Result<AppConfig> {
    let default = default_config()?;
    Ok(AppConfig {
        project: user.project.or(default.project),
        report: user.report,
        commands: {
            let mut cmds = default.commands;
            cmds.extend(user.commands);
            cmds
        },
        scoring: ScoringConfig {
            enabled: user.scoring.enabled,
            display_individual_scores: user.scoring.display_individual_scores,
            display_weighted_score: user.scoring.display_weighted_score,
            penalties: user.scoring.penalties,
            consensus_threshold: user.scoring.consensus_threshold,
            risk_thresholds: user.scoring.risk_thresholds,
        },
        review_experts: {
            let mut experts = default.review_experts;
            experts.extend(user.review_experts);
            experts
        },
        llm: if user.llm.is_empty() { default.llm } else { user.llm },
        output_dir: if user.output_dir.is_empty() {
            default.output_dir
        } else {
            user.output_dir
        },
        max_team_size: user.max_team_size.or(default.max_team_size),
        max_concurrent_llm_calls: user.max_concurrent_llm_calls.or(default.max_concurrent_llm_calls),
        diff: user.diff,
        rate_limit: user.rate_limit,
        languages: user.languages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::resolver::load_and_apply;

    #[test]
    fn test_merge_default_with_llm_config() {
        let user_toml = r#"
[[llm]]
provider = "openai"
model = "gpt-4"
api_key = "sk-test"
"#;
        let cfg = load_and_apply(user_toml).unwrap();
        assert_eq!(cfg.llm.len(), 1);
        assert_eq!(cfg.llm[0].provider, "openai");
        assert_eq!(cfg.llm[0].model, "gpt-4");
    }

    #[test]
    fn test_merge_default_llm_empty_when_not_set() {
        let cfg = default_config().unwrap();
        assert!(cfg.llm.is_empty());
    }

    #[test]
    fn test_merge_default_with_output_dir() {
        let user_toml = r#"
output_dir = "/custom/reports"
"#;
        let cfg = load_and_apply(user_toml).unwrap();
        assert_eq!(cfg.output_dir, "/custom/reports");
    }

    #[test]
    fn test_merge_default_output_dir_default() {
        let cfg = default_config().unwrap();
        // Should not be empty (defaults to ~/.config/review-engine/reports or similar)
        assert!(!cfg.output_dir.is_empty());
    }

    #[test]
    fn test_default_config_loads() {
        let cfg = default_config().unwrap();
        assert!(cfg.review_experts.contains_key("lead"));
        assert!(cfg.review_experts.contains_key("security"));
        assert!(cfg.review_experts.contains_key("performance"));
        assert!(cfg.review_experts.contains_key("quality"));
        assert!(cfg.review_experts.contains_key("reuse"));
        assert!(cfg.review_experts.contains_key("docs"));
        assert!(cfg.review_experts.contains_key("ux"));
        assert!(cfg.review_experts.contains_key("database"));
        assert!(cfg.review_experts.contains_key("devops"));
        assert!(cfg.review_experts.contains_key("api"));
        assert!(cfg.review_experts.contains_key("dependency"));
        assert!(cfg.review_experts.contains_key("aggregator"));
        assert!(!cfg.review_experts.get("aggregator").unwrap().enabled);
    }

    #[test]
    fn test_merge_default_merges_commands() {
        let user_toml = r#"
[review_experts]

[commands]
review = true
describe = true
"#;
        let user = parse_toml(user_toml).unwrap();
        let merged = merge_default(user).unwrap();
        assert!(*merged.commands.get("review").unwrap());
        assert!(*merged.commands.get("describe").unwrap());
        assert!(!*merged.commands.get("improve").unwrap());
        assert!(!*merged.commands.get("ask").unwrap());
    }

    // Empty config falls back to defaults because all fields have #[serde(default)].
    #[test]
    fn test_parse_toml_empty_uses_defaults() {
        let cfg = parse_toml("").unwrap();
        assert!(cfg.review_experts.is_empty());
        assert!(cfg.commands.is_empty());
    }

    // ─── DiffConfig tests ───────────────────────

    #[test]
    fn test_diff_config_defaults() {
        let cfg = default_config().unwrap();
        assert_eq!(cfg.diff.max_input_tokens, 120000);
        assert_eq!(cfg.diff.max_tokens_per_chunk, 30000);
        assert_eq!(cfg.diff.large_pr_file_threshold, 21);
        assert_eq!(cfg.diff.large_pr_line_threshold, 1000);
        assert_eq!(cfg.diff.compression_level, "aggressive");
        assert_eq!(cfg.diff.chunking_strategy, "adaptive");
        assert_eq!(cfg.diff.max_chunks_per_expert, 3);
    }

    #[test]
    fn test_rate_limit_config_defaults() {
        let cfg = default_config().unwrap();
        assert_eq!(cfg.rate_limit.max_rpm, 60);
        assert_eq!(cfg.rate_limit.max_tpm, 200000);
        assert_eq!(cfg.rate_limit.window_seconds, 60);
    }

    #[test]
    fn test_diff_config_parse() {
        let toml_str = r#"
[diff]
max_input_tokens = 50000
max_tokens_per_chunk = 10000
large_pr_file_threshold = 15
large_pr_line_threshold = 500
compression_level = "moderate"
chunking_strategy = "files"
max_chunks_per_expert = 5
"#;
        let cfg = parse_toml(toml_str).unwrap();
        assert_eq!(cfg.diff.max_input_tokens, 50000);
        assert_eq!(cfg.diff.max_tokens_per_chunk, 10000);
        assert_eq!(cfg.diff.large_pr_file_threshold, 15);
        assert_eq!(cfg.diff.large_pr_line_threshold, 500);
        assert_eq!(cfg.diff.compression_level, "moderate");
        assert_eq!(cfg.diff.chunking_strategy, "files");
        assert_eq!(cfg.diff.max_chunks_per_expert, 5);
    }

    #[test]
    fn test_rate_limit_config_parse() {
        let toml_str = r#"
[rate_limit]
max_rpm = 30
max_tpm = 100000
window_seconds = 120
"#;
        let cfg = parse_toml(toml_str).unwrap();
        assert_eq!(cfg.rate_limit.max_rpm, 30);
        assert_eq!(cfg.rate_limit.max_tpm, 100000);
        assert_eq!(cfg.rate_limit.window_seconds, 120);
    }

    // ─── parse_toml edge cases ───────────────────

    #[test]
    fn test_parse_toml_empty_input_uses_defaults() {
        let cfg = parse_toml("").unwrap();
        // All fields should have their serde defaults
        assert!(cfg.review_experts.is_empty());
        assert!(cfg.commands.is_empty());
        assert!(cfg.llm.is_empty());
        assert!(cfg.project.is_none());
        assert!(cfg.scoring.enabled); // default ScoringConfig has enabled: true
                                      // Actually ScoringConfig default has enabled: true, but parse_toml("") gives all defaults
                                      // Let me just check that it doesn't panic
        assert_eq!(cfg.diff.max_input_tokens, 120000);
        assert_eq!(cfg.diff.chunking_strategy, "adaptive");
    }

    #[test]
    fn test_parse_toml_partial_config() {
        let toml_str = r#"
[project]
name = "test-project"
"#;
        let cfg = parse_toml(toml_str).unwrap();
        assert_eq!(cfg.project.unwrap().name.unwrap(), "test-project");
        // Other fields should have defaults
        assert!(cfg.commands.is_empty());
        assert_eq!(cfg.diff.max_input_tokens, 120000);
    }

    #[test]
    fn test_parse_toml_invalid_toml_fails() {
        let result = parse_toml("[[[invalid]]]");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_toml_unknown_fields_ignored() {
        // Fields not in AppConfig should be ignored by serde(default)
        let toml_str = r#"
unknown_field = "value"
[unknown_section]
key = "val"
"#;
        let cfg = parse_toml(toml_str).unwrap();
        // Should parse successfully with defaults
        assert!(cfg.commands.is_empty());
    }

    // ─── merge_default with partial configs ──────

    #[test]
    fn test_merge_default_partial_review_experts() {
        // User provides one expert override; defaults should fill the rest
        let user_toml = r#"
[review_experts.lead]
enabled = false
weight = 20
"#;
        let user = parse_toml(user_toml).unwrap();
        let merged = merge_default(user).unwrap();
        // The lead expert should be overridden (disabled)
        assert!(!merged.review_experts.get("lead").unwrap().enabled);
        // Other default experts should still be present
        assert!(merged.review_experts.contains_key("security"));
        assert!(merged.review_experts.contains_key("quality"));
    }

    #[test]
    fn test_merge_default_llm_configs_preserved() {
        let user_toml = r#"
[[llm]]
provider = "openai"
model = "gpt-4"
api_key = "sk-test"
"#;
        let user = parse_toml(user_toml).unwrap();
        let merged = merge_default(user).unwrap();
        assert_eq!(merged.llm.len(), 1);
        assert_eq!(merged.llm[0].provider, "openai");
    }

    #[test]
    fn test_merge_default_llm_empty_keeps_default() {
        let user_toml = r#"
output_dir = "/custom"
"#;
        let user = parse_toml(user_toml).unwrap();
        let merged = merge_default(user).unwrap();
        // No llm in user config, so default (empty) should be used
        assert!(merged.llm.is_empty());
        assert_eq!(merged.output_dir, "/custom");
    }

    #[test]
    fn test_merge_default_with_scoring_config() {
        let user_toml = r#"
[scoring]
enabled = false
"#;
        let user = parse_toml(user_toml).unwrap();
        let merged = merge_default(user).unwrap();
        assert!(!merged.scoring.enabled);
        // display flags should be false (user's ScoringConfig default)
        // Actually: user's scoring only overrides 'enabled'; display_*
        // are defaulted by serde to true, but since user's struct is
        // constructed from TOML with default, display fields are true
        assert!(merged.scoring.display_individual_scores);
    }

    #[test]
    fn test_merge_default_output_dir_fallback() {
        let user_toml = r#"
[review_experts]
"#;
        let user = parse_toml(user_toml).unwrap();
        let merged = merge_default(user).unwrap();
        // output_dir not set in user, should use default
        assert!(!merged.output_dir.is_empty());
    }

    #[test]
    fn test_merge_default_commands_extend() {
        let user_toml = r#"
[commands]
custom_cmd = true
"#;
        let user = parse_toml(user_toml).unwrap();
        let merged = merge_default(user).unwrap();
        assert!(*merged.commands.get("custom_cmd").unwrap());
        // Default commands should still exist
        assert!(merged.commands.contains_key("review"));
        assert!(merged.commands.contains_key("describe"));
    }

    #[test]
    fn test_merge_default_max_team_size() {
        let user_toml = "max_team_size = 5\n";
        let user = parse_toml(user_toml).unwrap();
        let merged = merge_default(user).unwrap();
        assert_eq!(merged.max_team_size, Some(5));
    }

    #[test]
    fn test_merge_default_max_team_size_default() {
        let user_toml = "";
        let user = parse_toml(user_toml).unwrap();
        let merged = merge_default(user).unwrap();
        assert_eq!(merged.max_team_size, None);
    }

    #[test]
    fn test_merge_default_scoring_config_preserved() {
        let user_toml = r#"
[scoring]
enabled = false
consensus_threshold = 85

[scoring.penalties]
critical = 50

[scoring.risk_thresholds]
critical_max = 25
"#;
        let user = parse_toml(user_toml).unwrap();
        let merged = merge_default(user).unwrap();
        assert!(!merged.scoring.enabled);
        assert_eq!(merged.scoring.consensus_threshold, 85);
        assert_eq!(merged.scoring.penalties.critical, 50);
        assert_eq!(merged.scoring.penalties.high, 15); // default preserved
        assert_eq!(merged.scoring.risk_thresholds.critical_max, 25);
        assert_eq!(merged.scoring.risk_thresholds.high_max, 60); // default preserved
    }

    // ─── load_embedded_default ───────────────────

    #[test]
    fn test_load_embedded_default() {
        let cfg = load_embedded_default().unwrap();
        assert!(cfg.review_experts.contains_key("lead"));
    }
}
