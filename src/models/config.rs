use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::expert::{ExpertDef, ExpertTomlDef};

// ─── App 配置 ───────────────────────────────

/// Top-level application configuration deserialised from TOML.
///
/// Contains project metadata, expert definitions (under `[review_experts]`),
/// command toggles, LLM provider configs, diff-processing parameters, and
/// rate-limit settings. Missing fields fall back to sensible defaults.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AppConfig {
    /// Optional project metadata (name, etc.).
    pub project: Option<ProjectConfig>,
    /// Report output configuration (aggregation, max findings per expert).
    #[serde(default)]
    pub report: ReportConfig,
    /// Map of expert name → TOML definition for all review experts.
    #[serde(rename = "review_experts", default)]
    pub review_experts: HashMap<String, ExpertTomlDef>,
    /// Map of command name → enabled/disabled toggle.
    #[serde(default)]
    pub commands: HashMap<String, bool>,
    /// Scoring behaviour configuration.
    #[serde(default)]
    pub scoring: ScoringConfig,
    /// Sequence of LLM provider configurations (used round-robin with fallback).
    #[serde(default)]
    pub llm: Vec<LLMConfig>,
    /// Maximum number of experts that can participate in a single review.
    #[serde(default)]
    pub max_team_size: Option<usize>,
    /// Maximum number of concurrent LLM API calls.
    #[serde(default)]
    pub max_concurrent_llm_calls: Option<usize>,
    /// Directory path for writing review report files.
    #[serde(default = "default_output_dir")]
    pub output_dir: String,
    /// Diff processing parameters (token limits, chunking, compression).
    #[serde(default)]
    pub diff: DiffConfig,
    /// Rate-limiting configuration for LLM API calls.
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
    /// Language-specific profiles for multi-language project support.
    #[serde(default)]
    pub languages: LanguagesConfig,
}

/// Optional project metadata block from the config file.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProjectConfig {
    /// Human-readable project name (display only).
    #[serde(default)]
    pub name: Option<String>,
    /// High-level project type or runtime category (e.g. "embedded", "web",
    /// "mobile", "backend", "desktop").
    #[serde(default)]
    pub project_type: Option<String>,
    /// Target operating system (e.g. "Linux", "RTOS", "bare-metal").
    #[serde(default)]
    pub os: Option<String>,
    /// Target CPU architecture (e.g. "ARM", "x86_64", "RISC-V").
    #[serde(default)]
    pub arch: Option<String>,
    /// Application domain or industry (e.g. "IoT", "fintech", "consumer").
    #[serde(default)]
    pub domain: Option<String>,
    /// Additional project constraints that affect review relevance
    /// (e.g. "single-threaded BLE stack, 64 KiB RAM").
    #[serde(default)]
    pub constraints: Option<String>,
}

/// Controls how review reports are generated and presented.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReportConfig {
    /// If `true`, an aggregator expert merges individual reports into one.
    #[serde(default = "default_aggregated")]
    pub aggregated: bool,
    /// Maximum number of findings each expert may return.
    #[serde(default = "default_max_findings")]
    pub max_findings_per_expert: usize,
    /// Minimum confidence score (0-10) for a finding to be included in the report.
    #[serde(default = "default_min_confidence")]
    pub min_confidence: u8,
    /// If `true`, findings below `min_confidence` are dropped instead of included.
    #[serde(default = "default_drop_low_confidence")]
    pub drop_low_confidence: bool,
    /// If `true`, an extra LLM verification pass re-checks each finding against
    /// the diff hunks, the referenced file's full content, and the changed-file
    /// list, dropping findings the evidence disproves. Off by default (extra cost).
    #[serde(default = "default_verification_pass")]
    pub verification_pass: bool,
    /// Maximum bytes of referenced file content injected into the verification prompt.
    #[serde(default = "default_verification_max_file_bytes")]
    pub verification_max_file_bytes: usize,
}

/// Parameters controlling how large diffs are processed and chunked.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DiffConfig {
    /// Maximum allowed total input tokens for a single diff.
    #[serde(default = "default_diff_max_input_tokens")]
    pub max_input_tokens: usize,
    /// Maximum tokens per diff chunk when splitting large diffs.
    #[serde(default = "default_diff_max_tokens_per_chunk")]
    pub max_tokens_per_chunk: usize,
    /// File count threshold above which a PR is considered "large".
    #[serde(default = "default_diff_large_pr_file_threshold")]
    pub large_pr_file_threshold: usize,
    /// Line count threshold above which a PR is considered "large".
    #[serde(default = "default_diff_large_pr_line_threshold")]
    pub large_pr_line_threshold: usize,
    /// Compression strategy: "aggressive", "moderate", or "none".
    #[serde(default = "default_diff_compression_level")]
    pub compression_level: String,
    /// Chunking strategy: "adaptive", "files", or "tokens".
    #[serde(default = "default_diff_chunking_strategy")]
    pub chunking_strategy: String,
    /// Maximum number of chunks sent to a single expert.
    #[serde(default = "default_diff_max_chunks_per_expert")]
    pub max_chunks_per_expert: usize,
}

/// Rate-limiting parameters for LLM API requests.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RateLimitConfig {
    /// Maximum requests per minute across all providers.
    #[serde(default = "default_rate_limit_max_rpm")]
    pub max_rpm: usize,
    /// Maximum tokens per minute across all providers.
    #[serde(default = "default_rate_limit_max_tpm")]
    pub max_tpm: usize,
    /// Rolling window in seconds for the rate-limit counters.
    #[serde(default = "default_rate_limit_window_seconds")]
    pub window_seconds: u64,
}

/// Per-language profile defining comment syntax, test patterns, style tools, etc.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct LanguageProfile {
    /// Language name (e.g. "Rust", "Python").
    #[serde(default)]
    pub name: String,
    /// Inline comment prefixes (e.g. `["//"]` for Rust, `["#"]` for Python).
    #[serde(default)]
    pub comment_prefixes: Vec<String>,
    /// Doc comment prefixes (e.g. `["///", "//!"]` for Rust, `["\"\"\""]` for Python).
    #[serde(default)]
    pub doc_prefixes: Vec<String>,
    /// File-path patterns that indicate a test file.
    #[serde(default)]
    pub test_patterns: Vec<String>,
    /// Style/ linter configuration files to check for this language.
    #[serde(default)]
    pub style_configs: Vec<String>,
    /// Naming convention hint for LLM prompts.
    #[serde(default)]
    pub naming_hint: String,
    /// Error-handling convention hint for LLM prompts.
    #[serde(default)]
    pub error_hint: String,
}

/// Language-specific profiles keyed by language name.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct LanguagesConfig {
    /// When set to a non-empty language name, overrides auto-detection.
    #[serde(default)]
    pub dominant: String,
    /// Per-language profiles.
    #[serde(default)]
    pub profiles: std::collections::HashMap<String, LanguageProfile>,
}

/// Configuration for the scoring/weighting system.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScoringConfig {
    /// Master toggle for computing and displaying review scores.
    #[serde(default = "default_scoring_enabled")]
    pub enabled: bool,
    /// Whether to show per-expert scores in the report.
    #[serde(default = "default_true")]
    pub display_individual_scores: bool,
    /// Whether to show the overall weighted score in the report.
    #[serde(default = "default_true")]
    pub display_weighted_score: bool,
    /// Penalty points for each severity level.
    #[serde(default)]
    pub penalties: PenaltyConfig,
    /// Consensus threshold for high-confidence findings.
    #[serde(default = "default_consensus_threshold")]
    pub consensus_threshold: u8,
    /// Risk level thresholds based on score ranges.
    #[serde(default)]
    pub risk_thresholds: RiskThresholdConfig,
}

/// Penalty points for each severity level.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PenaltyConfig {
    pub critical: u8,
    pub high: u8,
    pub medium: u8,
    pub low: u8,
    pub note: u8,
}

impl Default for PenaltyConfig {
    fn default() -> Self {
        Self {
            critical: 30,
            high: 15,
            medium: 5,
            low: 1,
            note: 0,
        }
    }
}

/// Risk level thresholds based on score ranges.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RiskThresholdConfig {
    pub critical_max: u8, // score <= this → Critical
    pub high_max: u8,     // score <= this → High
    pub medium_max: u8,   // score <= this → Medium
    pub low_max: u8,      // score <= this → Low
}

impl Default for RiskThresholdConfig {
    fn default() -> Self {
        Self {
            critical_max: 40,
            high_max: 60,
            medium_max: 80,
            low_max: 95,
        }
    }
}

/// Configuration for a single LLM provider connection.
///
/// Multiple `LLMConfig` entries can be specified in config under `[[llm]]`;
/// they are tried in order with fallback on failure.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct LLMConfig {
    /// Provider name (e.g. `"openai"`, `"anthropic"`, `"ollama"`).
    pub provider: String,
    /// Model identifier (e.g. `"gpt-4"`, `"claude-3-opus"`).
    pub model: String,
    /// API key / authentication token.
    #[serde(default)]
    pub api_key: String,
    /// Base URL for the provider API (e.g. `"https://api.openai.com/v1"`).
    /// Also accepts `base_url` as an alias.
    #[serde(default, alias = "base_url")]
    pub api_base: String,
    /// Maximum number of tokens in the LLM response.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Sampling temperature (0.0–1.0; lower = more deterministic).
    #[serde(default = "default_temperature")]
    pub temperature: f32,
}

impl std::fmt::Debug for LLMConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LLMConfig")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("api_key", &"***")
            .field("api_base", &self.api_base)
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .finish()
    }
}

// ─── 配置来源 ──────────────────────────────

/// The origin of the application configuration.
///
/// Can be provided inline as a TOML string, loaded from a file path,
/// or auto-detected (by [`crate::config::resolve_config`]).
#[derive(Debug, Clone)]
pub enum ConfigSource {
    /// Raw TOML string provided programmatically.
    Inline(String),
    /// Path to a `.toml` configuration file on disk.
    Path(String),
}

// ─── Default impls ──────────────────────────

impl Default for DiffConfig {
    fn default() -> Self {
        Self {
            max_input_tokens: default_diff_max_input_tokens(),
            max_tokens_per_chunk: default_diff_max_tokens_per_chunk(),
            large_pr_file_threshold: default_diff_large_pr_file_threshold(),
            large_pr_line_threshold: default_diff_large_pr_line_threshold(),
            compression_level: default_diff_compression_level(),
            chunking_strategy: default_diff_chunking_strategy(),
            max_chunks_per_expert: default_diff_max_chunks_per_expert(),
        }
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_rpm: default_rate_limit_max_rpm(),
            max_tpm: default_rate_limit_max_tpm(),
            window_seconds: default_rate_limit_window_seconds(),
        }
    }
}

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            display_individual_scores: true,
            display_weighted_score: true,
            penalties: PenaltyConfig::default(),
            consensus_threshold: default_consensus_threshold(),
            risk_thresholds: RiskThresholdConfig::default(),
        }
    }
}

impl Default for ReportConfig {
    fn default() -> Self {
        Self {
            aggregated: false,
            max_findings_per_expert: 5,
            min_confidence: 6,
            drop_low_confidence: false,
            verification_pass: false,
            verification_max_file_bytes: 20000,
        }
    }
}

// ─── Default value functions ────────────────

fn default_output_dir() -> String {
    home::home_dir()
        .map(|p| {
            p.join(".config")
                .join("review-engine")
                .join("reports")
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_else(|| String::from(".config/review-engine/reports"))
}

fn default_diff_max_input_tokens() -> usize {
    120000
}
fn default_diff_max_tokens_per_chunk() -> usize {
    30000
}
fn default_diff_large_pr_file_threshold() -> usize {
    21
}
fn default_diff_large_pr_line_threshold() -> usize {
    1000
}
fn default_diff_compression_level() -> String {
    "aggressive".to_string()
}
fn default_diff_chunking_strategy() -> String {
    "adaptive".to_string()
}
fn default_diff_max_chunks_per_expert() -> usize {
    3
}

fn default_rate_limit_max_rpm() -> usize {
    60
}
fn default_rate_limit_max_tpm() -> usize {
    200000
}
fn default_rate_limit_window_seconds() -> u64 {
    60
}

fn default_aggregated() -> bool {
    false
}
fn default_max_findings() -> usize {
    5
}
fn default_min_confidence() -> u8 {
    6
}
fn default_drop_low_confidence() -> bool {
    false
}
fn default_verification_pass() -> bool {
    false
}
fn default_verification_max_file_bytes() -> usize {
    20000
}

fn default_scoring_enabled() -> bool {
    true
}
fn default_true() -> bool {
    true
}
fn default_consensus_threshold() -> u8 {
    70
}

fn default_max_tokens() -> u32 {
    4096
}
fn default_temperature() -> f32 {
    0.3
}

impl AppConfig {
    /// Build a list of enabled [`ExpertDef`] instances from the config.
    ///
    /// Filters out disabled experts and skips any that fail validation
    /// (with a warning log). The returned order is stable and matches
    /// the iteration order of the underlying HashMap.
    pub fn build_expert_defs(&self) -> Vec<ExpertDef> {
        self.review_experts
            .iter()
            .filter(|(_, e)| e.enabled)
            .filter_map(|(name, e)| {
                if let Err(err) = e.validate(name) {
                    tracing::warn!("{}; skipping expert '{}'", err, name);
                    None
                } else {
                    Some(ExpertDef::from((name, e)))
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── build_expert_defs ───────────────────────

    fn make_expert(enabled: bool) -> ExpertTomlDef {
        ExpertTomlDef {
            enabled,
            model: "test-model".to_string(),
            title: "Test Title".to_string(),
            role: "Test Role".to_string(),
            style: "test style".to_string(),
            principles: vec!["principle1".to_string()],
            focus: vec!["focus1".to_string()],
            standards: vec!["standard1".to_string()],
            weight: 10,
            commands: vec!["review".to_string()],
            trigger: None,
            prompt: Some("prompt".to_string()),
        }
    }

    #[test]
    fn test_build_expert_defs_filters_disabled() {
        let mut experts = HashMap::new();
        experts.insert("enabled_expert".to_string(), make_expert(true));
        experts.insert("disabled_expert".to_string(), make_expert(false));

        let config = AppConfig {
            project: None,
            report: ReportConfig::default(),
            review_experts: experts,
            commands: HashMap::new(),
            scoring: ScoringConfig::default(),
            llm: Vec::new(),
            output_dir: String::new(),
            max_team_size: None,
            max_concurrent_llm_calls: None,
            diff: DiffConfig::default(),
            rate_limit: RateLimitConfig::default(),
            languages: LanguagesConfig::default(),
        };

        let defs = config.build_expert_defs();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "enabled_expert");
    }

    #[test]
    fn test_project_config_parses_all_fields() {
        let config: AppConfig = toml::from_str(
            r#"
[project]
name = "review-engine"
project_type = "embedded"
os = "Linux"
arch = "ARM"
domain = "IoT"
constraints = "single-threaded BLE stack, 64 KiB RAM"
"#,
        )
        .unwrap();

        let project = config.project.expect("project should be present");
        assert_eq!(project.name, Some("review-engine".to_string()));
        assert_eq!(project.project_type, Some("embedded".to_string()));
        assert_eq!(project.os, Some("Linux".to_string()));
        assert_eq!(project.arch, Some("ARM".to_string()));
        assert_eq!(project.domain, Some("IoT".to_string()));
        assert_eq!(
            project.constraints,
            Some("single-threaded BLE stack, 64 KiB RAM".to_string())
        );
    }

    #[test]
    fn test_project_config_missing_fields_default_to_none() {
        let config: AppConfig = toml::from_str(
            r#"
[project]
name = "minimal"
"#,
        )
        .unwrap();

        let project = config.project.expect("project should be present");
        assert_eq!(project.name, Some("minimal".to_string()));
        assert!(project.project_type.is_none());
        assert!(project.os.is_none());
        assert!(project.arch.is_none());
        assert!(project.domain.is_none());
        assert!(project.constraints.is_none());
    }

    #[test]
    fn test_project_config_omitted() {
        let config: AppConfig = toml::from_str("").unwrap();
        assert!(config.project.is_none());
    }

    #[test]
    fn test_scoring_config_defaults() {
        let config: AppConfig = toml::from_str("").unwrap();
        assert!(config.scoring.enabled);
        assert!(config.scoring.display_individual_scores);
        assert!(config.scoring.display_weighted_score);
        assert_eq!(config.scoring.consensus_threshold, 70);
        assert_eq!(config.scoring.penalties.critical, 30);
        assert_eq!(config.scoring.penalties.high, 15);
        assert_eq!(config.scoring.penalties.medium, 5);
        assert_eq!(config.scoring.penalties.low, 1);
        assert_eq!(config.scoring.penalties.note, 0);
        assert_eq!(config.scoring.risk_thresholds.critical_max, 40);
        assert_eq!(config.scoring.risk_thresholds.high_max, 60);
        assert_eq!(config.scoring.risk_thresholds.medium_max, 80);
        assert_eq!(config.scoring.risk_thresholds.low_max, 95);
    }

    #[test]
    fn test_scoring_config_custom_values() {
        let config: AppConfig = toml::from_str(
            r#"
[scoring]
enabled = false
consensus_threshold = 80

[scoring.penalties]
critical = 50
high = 25
medium = 10
low = 2
note = 0

[scoring.risk_thresholds]
critical_max = 30
high_max = 50
medium_max = 70
low_max = 90
"#,
        )
        .unwrap();

        assert!(!config.scoring.enabled);
        assert_eq!(config.scoring.consensus_threshold, 80);
        assert_eq!(config.scoring.penalties.critical, 50);
        assert_eq!(config.scoring.penalties.high, 25);
        assert_eq!(config.scoring.penalties.medium, 10);
        assert_eq!(config.scoring.penalties.low, 2);
        assert_eq!(config.scoring.penalties.note, 0);
        assert_eq!(config.scoring.risk_thresholds.critical_max, 30);
        assert_eq!(config.scoring.risk_thresholds.high_max, 50);
        assert_eq!(config.scoring.risk_thresholds.medium_max, 70);
        assert_eq!(config.scoring.risk_thresholds.low_max, 90);
    }

    #[test]
    fn test_scoring_config_partial_penalties() {
        let config: AppConfig = toml::from_str(
            r#"
[scoring.penalties]
critical = 50
"#,
        )
        .unwrap();

        assert_eq!(config.scoring.penalties.critical, 50);
        assert_eq!(config.scoring.penalties.high, 15); // default
        assert_eq!(config.scoring.penalties.medium, 5); // default
    }

    #[test]
    fn test_scoring_config_partial_risk_thresholds() {
        let config: AppConfig = toml::from_str(
            r#"
[scoring.risk_thresholds]
critical_max = 20
low_max = 85
"#,
        )
        .unwrap();

        assert_eq!(config.scoring.risk_thresholds.critical_max, 20);
        assert_eq!(config.scoring.risk_thresholds.high_max, 60); // default
        assert_eq!(config.scoring.risk_thresholds.medium_max, 80); // default
        assert_eq!(config.scoring.risk_thresholds.low_max, 85);
    }

    // ─── ReportConfig ────────────────────────────

    #[test]
    fn test_report_config_defaults() {
        let config: AppConfig = toml::from_str("").unwrap();
        assert!(!config.report.aggregated);
        assert_eq!(config.report.max_findings_per_expert, 5);
        assert_eq!(config.report.min_confidence, 6);
        assert!(!config.report.drop_low_confidence);
        assert!(!config.report.verification_pass);
        assert_eq!(config.report.verification_max_file_bytes, 20000);
    }

    #[test]
    fn test_report_config_custom_values() {
        let config: AppConfig = toml::from_str(
            r#"
[report]
aggregated = true
max_findings_per_expert = 10
min_confidence = 7
drop_low_confidence = true
verification_pass = true
verification_max_file_bytes = 50000
"#,
        )
        .unwrap();

        assert!(config.report.aggregated);
        assert_eq!(config.report.max_findings_per_expert, 10);
        assert_eq!(config.report.min_confidence, 7);
        assert!(config.report.drop_low_confidence);
        assert!(config.report.verification_pass);
        assert_eq!(config.report.verification_max_file_bytes, 50000);
    }
}
