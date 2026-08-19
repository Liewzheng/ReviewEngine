use super::*;
use crate::config::defaults::default_config;
use std::sync::Mutex;

/// Helper: returns a TOML string with all default experts disabled.
/// Tests append their own expert sections to override specific ones.
fn base_disabled_toml() -> String {
    r#"
[review_experts.lead]
enabled = false
weight = 20

[review_experts.security]
enabled = false
weight = 15

[review_experts.performance]
enabled = false
weight = 10

[review_experts.quality]
enabled = false
weight = 10

[review_experts.reuse]
enabled = false
weight = 12

[review_experts.docs]
enabled = false
weight = 5

[review_experts.ux]
enabled = false
weight = 8

[review_experts.database]
enabled = false
weight = 5

[review_experts.devops]
enabled = false
weight = 5

[review_experts.api]
enabled = false
weight = 5

[review_experts.dependency]
enabled = false
weight = 5
"#
    .to_string()
}

struct EnvGuard {
    key: &'static str,
    original: Option<String>,
}

impl EnvGuard {
    fn new(key: &'static str) -> Self {
        let original = std::env::var(key).ok();
        Self { key, original }
    }

    fn set(&self, value: &str) {
        std::env::set_var(self.key, value);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.original.take() {
            Some(val) => std::env::set_var(self.key, val),
            None => std::env::remove_var(self.key),
        }
    }
}

/// Capture and clear the `CODE_AUDIT_*` override variables, restoring
/// their original values on drop. Tests that resolve configuration
/// through `apply_env_overrides` use this so a variable leaked from the
/// process environment (or left over by an unlocked test) cannot skew
/// assertions. Must only be called while holding [`fs_lock()`].
fn clear_code_audit_env() -> (EnvGuard, EnvGuard) {
    let commands = EnvGuard::new("CODE_AUDIT_COMMANDS");
    std::env::remove_var("CODE_AUDIT_COMMANDS");
    let scoring = EnvGuard::new("CODE_AUDIT_SCORING_ENABLED");
    std::env::remove_var("CODE_AUDIT_SCORING_ENABLED");
    (commands, scoring)
}

#[test]
fn test_apply_env_overrides_invalid_toml() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("CODE_AUDIT_COMMANDS");
    env_guard.set("not valid toml {{{");

    let config = default_config().unwrap();
    let overridden = apply_env_overrides(config);
    // Invalid TOML should be silently ignored, commands unchanged
    assert_eq!(overridden.commands.len(), 6);
}

#[test]
fn test_apply_env_overrides_scoring_non_bool() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("CODE_AUDIT_SCORING_ENABLED");
    env_guard.set("not_a_boolean");

    let config = default_config().unwrap();
    let overridden = apply_env_overrides(config);
    // Non-boolean should be treated as false
    assert!(!overridden.scoring.enabled);
}

#[test]
fn test_apply_env_overrides_commands() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("CODE_AUDIT_COMMANDS");
    env_guard.set("review = true\ndescribe = true");

    let config = default_config().unwrap();
    // review is true by default; describe is false by default
    assert!(*config.commands.get("review").unwrap());
    assert!(!*config.commands.get("describe").unwrap());

    let overridden = apply_env_overrides(config);
    assert!(*overridden.commands.get("review").unwrap());
    assert!(*overridden.commands.get("describe").unwrap());
}

#[test]
fn test_apply_env_overrides_empty_commands() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("CODE_AUDIT_COMMANDS");
    env_guard.set("");

    let config = default_config().unwrap();
    let overridden = apply_env_overrides(config);
    // Empty string is invalid TOML and should be silently ignored
    assert_eq!(overridden.commands.len(), 6);
}

#[test]
fn test_validate_experts_weight_sum_not_100() {
    let _guard = fs_lock();
    let user_toml = r#"
[review_experts.lead]
enabled = true
weight = 10

[review_experts.security]
enabled = false

[review_experts.performance]
enabled = false

[review_experts.quality]
enabled = false

[review_experts.reuse]
enabled = false

[review_experts.docs]
enabled = false

[review_experts.ux]
enabled = false

[review_experts.database]
enabled = false

[review_experts.devops]
enabled = false

[review_experts.api]
enabled = false

[review_experts.dependency]
enabled = false
"#
    .to_string();
    let result = load_and_apply(&user_toml);
    match result {
        Ok(_) => panic!("Expected validation error for weight sum != 100"),
        Err(e) => assert!(
            e.to_string().contains("sum to"),
            "Error should mention weight sum: {}",
            e
        ),
    }
}

#[test]
fn test_validate_experts_weight_sum_100() {
    let _guard = fs_lock();
    // Explicit config — disable all defaults, use only alice+bob = 100
    let user_toml = format!(
        "{}[review_experts.alice]\nenabled = true\nweight = 60\n\n[review_experts.bob]\nenabled = true\nweight = 40\n",
        base_disabled_toml()
    );
    let cfg = load_and_apply(&user_toml).unwrap();
    validate_experts(&cfg).unwrap();
}

#[test]
fn test_validate_experts_no_enabled_experts() {
    let _guard = fs_lock();
    let user_toml = base_disabled_toml();
    let cfg = load_and_apply(&user_toml).unwrap();
    validate_experts(&cfg).unwrap();
}

#[test]
fn test_validate_experts_zero_weight_sum() {
    let _guard = fs_lock();
    let user_toml = r#"
[review_experts.lead]
enabled = true
weight = 0

[review_experts.security]
enabled = true
weight = 0

[review_experts.performance]
enabled = false

[review_experts.quality]
enabled = false

[review_experts.reuse]
enabled = false

[review_experts.docs]
enabled = false

[review_experts.ux]
enabled = false

[review_experts.database]
enabled = false

[review_experts.devops]
enabled = false

[review_experts.api]
enabled = false

[review_experts.dependency]
enabled = false
"#
    .to_string();
    let cfg = load_and_apply(&user_toml).unwrap();
    // Only lead(0) + security(0) enabled, sum = 0, should pass validation
    validate_experts(&cfg).unwrap();
}

#[tokio::test]
async fn test_resolve_config_path_not_found() {
    let result = resolve_config(Some(ConfigSource::Path(
        "/tmp/nonexistent_config_file_12345.toml".to_string(),
    )))
    .await;
    assert!(result.is_err());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_default_path() {
    // resolve_config(None) reads the user-level config from $HOME and the
    // project-level config from the current directory (both process-wide),
    // and may push into FALLBACK_WARNINGS. Hold FS_LOCK and redirect all
    // of them so parallel tests (and the developer's real ~/.config or a
    // stray project-level file) cannot interfere.
    let _guard = fs_lock();
    let _env = clear_code_audit_env();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    let _home_guard = HomeGuard::set(&home);
    let _cwd_guard = CwdGuard::set(&project_dir);
    let cfg = resolve_config(None).await.unwrap();
    assert!(cfg.review_experts.contains_key("lead"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_inline() {
    let _guard = fs_lock();
    let _env = clear_code_audit_env();
    let cfg = resolve_config(Some(ConfigSource::Inline("[commands]\nreview = true".to_string())))
        .await
        .unwrap();
    assert!(*cfg.commands.get("review").unwrap());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_with_invalid_weight_sum() {
    let _guard = fs_lock();
    let _env = clear_code_audit_env();
    // TOML that overrides lead.weight to make sum != 100
    let toml = r#"
[review_experts.lead]
weight = 99
"#;
    let result = resolve_config(Some(ConfigSource::Inline(toml.to_string()))).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("sum to"), "Error should mention weight sum: {}", err);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_inline_full_toml() {
    let _guard = fs_lock();
    let _env = clear_code_audit_env();
    let toml = r#"
output_dir = "/tmp/test-reports"

[commands]
review = true
describe = true
"#;
    let cfg = resolve_config(Some(ConfigSource::Inline(toml.to_string())))
        .await
        .unwrap();
    assert_eq!(cfg.output_dir, "/tmp/test-reports");
    assert!(*cfg.commands.get("review").unwrap());
    assert!(*cfg.commands.get("describe").unwrap());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_inline_empty_still_defaults() {
    let _guard = fs_lock();
    let _env = clear_code_audit_env();
    let cfg = resolve_config(Some(ConfigSource::Inline(String::new()))).await.unwrap();
    // With empty inline, should parse with all defaults
    assert!(cfg.review_experts.contains_key("lead"));
    assert_eq!(cfg.diff.max_input_tokens, 120000);
}

#[test]
fn test_apply_env_overrides_enables_scoring() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("CODE_AUDIT_SCORING_ENABLED");
    env_guard.set("true");

    let config = default_config().unwrap();
    let overridden = apply_env_overrides(config);
    assert!(overridden.scoring.enabled);
}

#[test]
fn test_apply_env_overrides_disables_scoring_with_0() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("CODE_AUDIT_SCORING_ENABLED");
    env_guard.set("0");

    let config = default_config().unwrap();
    let overridden = apply_env_overrides(config);
    assert!(!overridden.scoring.enabled);
}

#[test]
fn test_apply_env_overrides_scoring_with_1() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("CODE_AUDIT_SCORING_ENABLED");
    env_guard.set("1");

    let config = default_config().unwrap();
    let overridden = apply_env_overrides(config);
    assert!(overridden.scoring.enabled);
}

#[test]
fn test_apply_env_overrides_unset_does_nothing() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("CODE_AUDIT_COMMANDS");
    // If the env var was set, restore original; otherwise leave it unset
    if std::env::var("CODE_AUDIT_COMMANDS").is_ok() {
        std::env::remove_var("CODE_AUDIT_COMMANDS");
    }
    // Drop and re-create to capture current state
    drop(env_guard);
    let _env_guard = EnvGuard::new("CODE_AUDIT_COMMANDS");

    let config = default_config().unwrap();
    let overridden = apply_env_overrides(config);
    // review is true by default; env var is unset so should remain true
    assert!(*overridden.commands.get("review").unwrap());
    // describe is false by default; env var is unset so should remain false
    assert!(!*overridden.commands.get("describe").unwrap());
}

#[test]
fn test_llm_configs_from_env_valid_json() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("LLM_CONFIG");
    env_guard.set(r#"[{"provider":"openai","model":"gpt-4o","api_key":"sk-env"}]"#);

    let configs = llm_configs_from_env();
    assert_eq!(configs.len(), 1);
    assert_eq!(configs[0].provider, "openai");
    assert_eq!(configs[0].model, "gpt-4o");
    assert_eq!(configs[0].api_key, "sk-env");
}

#[test]
fn test_llm_configs_from_env_invalid_json_returns_empty() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("LLM_CONFIG");
    env_guard.set("not valid json {{{");

    assert!(llm_configs_from_env().is_empty());
}

#[test]
fn test_llm_configs_from_env_unset_returns_empty() {
    let _guard = fs_lock();
    let _env_guard = EnvGuard::new("LLM_CONFIG");
    std::env::remove_var("LLM_CONFIG");

    assert!(llm_configs_from_env().is_empty());
}

#[test]
fn test_llm_configs_from_env_empty_values_return_empty() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("LLM_CONFIG");

    env_guard.set("");
    assert!(llm_configs_from_env().is_empty());

    env_guard.set("[]");
    assert!(llm_configs_from_env().is_empty());
}

#[test]
fn test_apply_llm_env_fallback_fills_empty_config() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("LLM_CONFIG");
    env_guard.set(r#"[{"provider":"openai","model":"gpt-4o","api_key":"sk-env"}]"#);

    let mut config = default_config().unwrap();
    assert!(config.llm.is_empty());
    apply_llm_env_fallback(&mut config);
    assert_eq!(config.llm.len(), 1);
    assert_eq!(config.llm[0].api_key, "sk-env");
}

#[test]
fn test_apply_llm_env_fallback_keeps_file_providers() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("LLM_CONFIG");
    env_guard.set(r#"[{"provider":"openai","model":"gpt-4o","api_key":"sk-env"}]"#);

    let mut config = default_config().unwrap();
    config.llm = vec![LLMConfig {
        provider: "anthropic".to_string(),
        model: "claude-sonnet-4-20250514".to_string(),
        api_key: "sk-file".to_string(),
        api_base: String::new(),
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: None,
    }];
    apply_llm_env_fallback(&mut config);
    // File-based providers win; the env var never overrides them.
    assert_eq!(config.llm.len(), 1);
    assert_eq!(config.llm[0].api_key, "sk-file");
}

#[test]
fn test_apply_llm_env_fallback_unset_keeps_empty() {
    let _guard = fs_lock();
    let _env_guard = EnvGuard::new("LLM_CONFIG");
    std::env::remove_var("LLM_CONFIG");

    let mut config = default_config().unwrap();
    apply_llm_env_fallback(&mut config);
    assert!(config.llm.is_empty());
}

#[test]
fn test_load_and_apply_full_flow() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("CODE_AUDIT_COMMANDS");
    env_guard.set("review = true\ndescribe = true");
    let env_guard2 = EnvGuard::new("CODE_AUDIT_SCORING_ENABLED");
    env_guard2.set("false");

    let user_toml = format!("output_dir = \"/tmp/test-reports\"\n\n[diff]\nmax_input_tokens = 50000\n\n{}\n\n[review_experts.alice]\nenabled = true\nweight = 100\n",
        base_disabled_toml().trim()
    );

    let cfg = load_and_apply(&user_toml).unwrap();
    assert_eq!(cfg.output_dir, "/tmp/test-reports");
    assert_eq!(cfg.diff.max_input_tokens, 50000);
    assert!(*cfg.commands.get("review").unwrap());
    assert!(*cfg.commands.get("describe").unwrap());
    assert!(!cfg.scoring.enabled);
}

#[test]
fn test_load_and_apply_env_override_precedence() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("CODE_AUDIT_SCORING_ENABLED");
    env_guard.set("false");

    let user_toml = r#"
[scoring]
enabled = true
"#;

    let cfg = load_and_apply(user_toml).unwrap();
    // Environment override should take precedence over TOML
    assert!(!cfg.scoring.enabled);
}

#[test]
fn test_validate_experts_weight_overflow() {
    let _guard = fs_lock();
    let user_toml = format!(
        "{}[review_experts.alice]\nenabled = true\nweight = 255\n",
        base_disabled_toml()
    );
    let parsed = parse_toml(&user_toml).unwrap();
    let merged = merge_default(parsed).unwrap();
    let cfg = apply_env_overrides(merged);
    // Weight sum is 255, which should fail validation (must be 100)
    let result = validate_experts(&cfg);
    assert!(result.is_err());
}

#[test]
fn test_apply_env_overrides_commands_multiple() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("CODE_AUDIT_COMMANDS");
    env_guard.set("review = true\ndescribe = true\nimprove = false\nask = true");

    let config = default_config().unwrap();
    let overridden = apply_env_overrides(config);
    assert!(*overridden.commands.get("review").unwrap());
    assert!(*overridden.commands.get("describe").unwrap());
    assert!(!*overridden.commands.get("improve").unwrap());
    assert!(*overridden.commands.get("ask").unwrap());
}

#[test]
fn test_load_and_apply_invalid_weight_expert() {
    let _guard = fs_lock();
    let user_toml = format!(
        "{}[review_experts.alice]\nenabled = true\nweight = 50\n[review_experts.bob]\nenabled = true\nweight = 60\n",
        base_disabled_toml()
    );
    let result = load_and_apply(&user_toml);
    assert!(result.is_err());
}

#[test]
fn test_load_and_apply_missing_required_model() {
    let _guard = fs_lock();
    // model is not required in ExpertTomlDef (it defaults to empty string)
    let user_toml = format!(
        "{}[review_experts.alice]\nenabled = true\nweight = 100\nrole = \"Test Role\"\n",
        base_disabled_toml()
    );
    let cfg = load_and_apply(&user_toml).unwrap();
    assert!(cfg.review_experts.contains_key("alice"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_with_scoring_penalties() {
    let _guard = fs_lock();
    let _env = clear_code_audit_env();
    let toml = r#"
[scoring.penalties]
critical = 50
high = 25
"#;
    let cfg = resolve_config(Some(ConfigSource::Inline(toml.to_string())))
        .await
        .unwrap();
    assert_eq!(cfg.scoring.penalties.critical, 50);
    assert_eq!(cfg.scoring.penalties.high, 25);
    // Defaults should be preserved for unspecified fields
    assert_eq!(cfg.scoring.penalties.medium, 5);
    assert_eq!(cfg.scoring.penalties.low, 1);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_with_risk_thresholds() {
    let _guard = fs_lock();
    let _env = clear_code_audit_env();
    let toml = r#"
[scoring.risk_thresholds]
critical_max = 30
high_max = 50
"#;
    let cfg = resolve_config(Some(ConfigSource::Inline(toml.to_string())))
        .await
        .unwrap();
    assert_eq!(cfg.scoring.risk_thresholds.critical_max, 30);
    assert_eq!(cfg.scoring.risk_thresholds.high_max, 50);
    assert_eq!(cfg.scoring.risk_thresholds.medium_max, 80); // default
    assert_eq!(cfg.scoring.risk_thresholds.low_max, 95); // default
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_with_consensus_threshold() {
    let _guard = fs_lock();
    let _env = clear_code_audit_env();
    let toml = r#"
[scoring]
consensus_threshold = 80
"#;
    let cfg = resolve_config(Some(ConfigSource::Inline(toml.to_string())))
        .await
        .unwrap();
    assert_eq!(cfg.scoring.consensus_threshold, 80);
}

#[test]
fn test_validate_experts_returns_ok_for_sum_100() {
    let _guard = fs_lock();
    let user_toml = format!(
        "{}[review_experts.alice]\nenabled = true\nweight = 50\n\n[review_experts.bob]\nenabled = true\nweight = 50\n",
        base_disabled_toml()
    );
    let cfg = load_and_apply(&user_toml).unwrap();
    assert!(validate_experts(&cfg).is_ok());
}

#[test]
fn test_validate_experts_err_for_sum_not_100() {
    let _guard = fs_lock();
    let user_toml = format!(
        "{}[review_experts.alice]\nenabled = true\nweight = 30\n",
        base_disabled_toml()
    );
    let result = load_and_apply(&user_toml);
    assert!(result.is_err());
}

#[test]
fn test_config_resolver_construct() {
    let resolver = ConfigResolver::new();
    // Verify the resolver is constructable and has the correct type
    let _: ConfigResolver = resolver;
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_inline_with_commands() {
    // resolve_config(Inline) pipes the TOML through apply_env_overrides,
    // which reads the process-wide CODE_AUDIT_* variables. Hold FS_LOCK
    // and clear them so parallel tests that set those variables cannot
    // flip the commands asserted below.
    let _guard = fs_lock();
    let _env = clear_code_audit_env();
    let toml = r#"
[commands]
improve = true
ask = true
"#;
    let cfg = resolve_config(Some(ConfigSource::Inline(toml.to_string())))
        .await
        .unwrap();
    assert!(*cfg.commands.get("improve").unwrap());
    assert!(*cfg.commands.get("ask").unwrap());
    // review is not specified in the TOML above, but the embedded default
    // sets it to true via the config file.  The test only checks that the
    // explicitly-set commands are present — review may be true or false
    // depending on the embedded defaults.
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_inline_with_diff_config() {
    let _guard = fs_lock();
    let _env = clear_code_audit_env();
    let toml = r#"
[diff]
compression_level = "none"
chunking_strategy = "files"
"#;
    let cfg = resolve_config(Some(ConfigSource::Inline(toml.to_string())))
        .await
        .unwrap();
    assert_eq!(cfg.diff.compression_level, "none");
    assert_eq!(cfg.diff.chunking_strategy, "files");
    assert_eq!(cfg.diff.max_input_tokens, 120000); // from default
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_inline_with_scoring() {
    let toml = r#"
[scoring]
enabled = false
display_individual_scores = false
"#;
    let _guard = fs_lock();
    let _env = clear_code_audit_env();

    let cfg = resolve_config(Some(ConfigSource::Inline(toml.to_string())))
        .await
        .unwrap();
    assert!(!cfg.scoring.enabled);
    assert!(!cfg.scoring.display_individual_scores);
}

#[test]
fn test_load_and_apply_empty_toml() {
    let _guard = fs_lock();
    let cfg = load_and_apply("").unwrap();
    assert!(cfg.review_experts.contains_key("lead"));
    assert_eq!(cfg.diff.max_input_tokens, 120000);
}

#[test]
fn test_load_and_apply_invalid_toml_fails() {
    let result = load_and_apply("[[[invalid]]]");
    assert!(result.is_err());
}

#[test]
fn test_apply_env_overrides_commands_partial() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("CODE_AUDIT_COMMANDS");
    env_guard.set("review = true");

    let mut config = default_config().unwrap();
    // Set a custom command to verify it survives the override
    config.commands.insert("custom_cmd".to_string(), true);
    let overridden = apply_env_overrides(config);
    assert!(*overridden.commands.get("review").unwrap());
    assert!(*overridden.commands.get("custom_cmd").unwrap());
}

#[test]
fn test_validate_experts_single_expert_weight_100() {
    let _guard = fs_lock();
    let user_toml = format!(
        "{}[review_experts.solo]\nenabled = true\nweight = 100\n",
        base_disabled_toml()
    );
    let cfg = load_and_apply(&user_toml).unwrap();
    assert!(validate_experts(&cfg).is_ok());
}

#[test]
fn test_load_and_apply_full_pipeline() {
    let _guard = fs_lock();
    let env_guard = EnvGuard::new("CODE_AUDIT_SCORING_ENABLED");
    env_guard.set("true");

    let user_toml = format!(
        "output_dir = \"/tmp/test-reports\"\n\n[commands]\nreview = true\n\n{}\n\n[review_experts.alice]\nenabled = true\nweight = 100\nrole = \"Test\"\ntitle = \"Test\"\nstyle = \"test\"\nprinciples = [\"p1\"]\nfocus = [\"f1\"]\nstandards = [\"s1\"]\nprompt = \"test\"\ncommands = [\"review\"]\n",
        base_disabled_toml().trim()
    );
    let cfg = load_and_apply(&user_toml).unwrap();
    assert_eq!(cfg.output_dir, "/tmp/test-reports");
    assert!(cfg.scoring.enabled);
    assert!(cfg.review_experts.contains_key("alice"));
}

#[test]
fn test_apply_env_overrides_multiple_vars() {
    let _guard = fs_lock();
    let cmd_guard = EnvGuard::new("CODE_AUDIT_COMMANDS");
    cmd_guard.set("review = true\ndescribe = true");
    let score_guard = EnvGuard::new("CODE_AUDIT_SCORING_ENABLED");
    score_guard.set("1");

    let config = default_config().unwrap();
    let overridden = apply_env_overrides(config);
    assert!(overridden.scoring.enabled);
    assert!(*overridden.commands.get("review").unwrap());
    assert!(*overridden.commands.get("describe").unwrap());
}

#[test]
fn test_validate_experts_fails_on_missing_role() {
    let _guard = fs_lock();
    // Expert without role is still accepted by ExpertTomlDef default (role defaults to "")
    // but validate does not check role. The validation only checks weights sum to 100.
    let user_toml = format!(
        "{}[review_experts.alice]\nenabled = true\nweight = 100\n",
        base_disabled_toml()
    );
    let cfg = load_and_apply(&user_toml).unwrap();
    assert!(validate_experts(&cfg).is_ok());
}

#[test]
fn test_load_and_apply_invalid_expert_weight_sum() {
    let _guard = fs_lock();
    let user_toml = format!(
        "{}[review_experts.alice]\nenabled = true\nweight = 30\n[review_experts.bob]\nenabled = true\nweight = 30\n",
        base_disabled_toml()
    );
    let result = load_and_apply(&user_toml);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("sum to"), "Error should mention weight sum: {}", err);
}

// ── LLM fallback tests ──────────────────────────────────────────────────

fn user_llm_toml() -> &'static str {
    r#"
[[llm]]
provider = "openai"
model = "gpt-4"
api_key = "user-key"
"#
}

/// Guard that temporarily sets `$HOME` and restores it on drop.
struct HomeGuard {
    original: Option<String>,
}

impl HomeGuard {
    fn set(path: &std::path::Path) -> Self {
        let original = std::env::var("HOME").ok();
        std::env::set_var("HOME", path.as_os_str());
        Self { original }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.original.take() {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// Guard that temporarily changes the current directory and restores it on drop.
struct CwdGuard {
    original: std::path::PathBuf,
}

impl CwdGuard {
    fn set(path: &std::path::Path) -> Self {
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(path).unwrap();
        Self { original }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.original).unwrap();
    }
}

/// Serializes every test that touches process-global state: the
/// `CODE_AUDIT_*` environment variables, `$HOME`, the current directory,
/// and `FALLBACK_WARNINGS`. Acquire it via [`fs_lock()`] *before*
/// creating any `EnvGuard` / `HomeGuard` / `CwdGuard`.
static FS_LOCK: Mutex<()> = Mutex::new(());

/// Acquire the global test lock, tolerating mutex poisoning. The guards
/// above restore process-global state on drop even when a test panics,
/// so a poisoned lock only means "another test already failed" —
/// ignoring the poison keeps one panicking test from cascading
/// `PoisonError` failures into every other serialized test.
fn fs_lock() -> std::sync::MutexGuard<'static, ()> {
    FS_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn clear_fallback_warnings() {
    let mut guard = super::resolve::FALLBACK_WARNINGS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    guard.clear();
}

fn take_fallback_warnings() -> Vec<String> {
    let mut guard = super::resolve::FALLBACK_WARNINGS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    guard.drain(..).collect()
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_path_uses_user_llm_fallback() {
    let _guard = fs_lock();
    clear_fallback_warnings();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let user_path = home.join(".config").join("review-engine");
    std::fs::create_dir_all(&user_path).unwrap();
    std::fs::write(user_path.join(".code-audit-config.toml"), user_llm_toml()).unwrap();

    let project_path = tmp.path().join("project.toml");
    std::fs::write(&project_path, "[commands]\nreview = true\n").unwrap();

    let _home_guard = HomeGuard::set(&home);
    let cfg = resolve_config(Some(ConfigSource::Path(project_path.to_string_lossy().to_string())))
        .await
        .unwrap();

    assert_eq!(cfg.llm.len(), 1);
    assert_eq!(cfg.llm[0].provider, "openai");
    assert_eq!(cfg.llm[0].model, "gpt-4");
    assert_eq!(cfg.llm[0].api_key, "user-key");

    let warnings = take_fallback_warnings();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("is missing"));
    assert!(warnings[0].contains(&*project_path.to_string_lossy()));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_path_invalid_project_llm_fallback() {
    let _guard = fs_lock();
    clear_fallback_warnings();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let user_path = home.join(".config").join("review-engine");
    std::fs::create_dir_all(&user_path).unwrap();
    std::fs::write(user_path.join(".code-audit-config.toml"), user_llm_toml()).unwrap();

    let project_path = tmp.path().join("project.toml");
    let project_toml = r#"
[llm]
provider = "openai"
model = "gpt-4"
"#;
    std::fs::write(&project_path, project_toml).unwrap();

    let _home_guard = HomeGuard::set(&home);
    let cfg = resolve_config(Some(ConfigSource::Path(project_path.to_string_lossy().to_string())))
        .await
        .unwrap();

    assert_eq!(cfg.llm.len(), 1);
    assert_eq!(cfg.llm[0].api_key, "user-key");

    let warnings = take_fallback_warnings();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("is invalid"));
    assert!(warnings[0].contains(&*project_path.to_string_lossy()));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_path_project_llm_wins() {
    let _guard = fs_lock();
    clear_fallback_warnings();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let user_path = home.join(".config").join("review-engine");
    std::fs::create_dir_all(&user_path).unwrap();
    std::fs::write(user_path.join(".code-audit-config.toml"), user_llm_toml()).unwrap();

    let project_path = tmp.path().join("project.toml");
    let project_toml = r#"
[[llm]]
provider = "anthropic"
model = "claude-3"
api_key = "project-key"
"#;
    std::fs::write(&project_path, project_toml).unwrap();

    let _home_guard = HomeGuard::set(&home);
    let cfg = resolve_config(Some(ConfigSource::Path(project_path.to_string_lossy().to_string())))
        .await
        .unwrap();

    assert_eq!(cfg.llm.len(), 1);
    assert_eq!(cfg.llm[0].provider, "anthropic");
    assert_eq!(cfg.llm[0].api_key, "project-key");

    let warnings = take_fallback_warnings();
    assert!(warnings.is_empty());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_none_project_missing_uses_user_llm() {
    let _guard = fs_lock();
    clear_fallback_warnings();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let user_path = home.join(".config").join("review-engine");
    std::fs::create_dir_all(&user_path).unwrap();
    std::fs::write(user_path.join(".code-audit-config.toml"), user_llm_toml()).unwrap();

    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join(".code-audit-config.toml"),
        "[commands]\nreview = true\n",
    )
    .unwrap();

    let _home_guard = HomeGuard::set(&home);
    let _cwd_guard = CwdGuard::set(&project_dir);
    let cfg = resolve_config(None).await.unwrap();

    assert_eq!(cfg.llm.len(), 1);
    assert_eq!(cfg.llm[0].api_key, "user-key");

    let warnings = take_fallback_warnings();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("is missing"));
    assert!(warnings[0].contains(".code-audit-config.toml"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_none_invalid_project_llm_fallback() {
    let _guard = fs_lock();
    clear_fallback_warnings();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let user_path = home.join(".config").join("review-engine");
    std::fs::create_dir_all(&user_path).unwrap();
    std::fs::write(user_path.join(".code-audit-config.toml"), user_llm_toml()).unwrap();

    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join(".code-audit-config.toml"),
        "[llm]\nprovider = \"openai\"\nmodel = \"gpt-4\"\n",
    )
    .unwrap();

    let _home_guard = HomeGuard::set(&home);
    let _cwd_guard = CwdGuard::set(&project_dir);
    let cfg = resolve_config(None).await.unwrap();

    assert_eq!(cfg.llm.len(), 1);
    assert_eq!(cfg.llm[0].api_key, "user-key");

    let warnings = take_fallback_warnings();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("is invalid"));
    assert!(warnings[0].contains(".code-audit-config.toml"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_none_project_llm_wins() {
    let _guard = fs_lock();
    clear_fallback_warnings();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let user_path = home.join(".config").join("review-engine");
    std::fs::create_dir_all(&user_path).unwrap();
    std::fs::write(user_path.join(".code-audit-config.toml"), user_llm_toml()).unwrap();

    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join(".code-audit-config.toml"),
        "[[llm]]\nprovider = \"anthropic\"\nmodel = \"claude-3\"\napi_key = \"project-key\"\n",
    )
    .unwrap();

    let _home_guard = HomeGuard::set(&home);
    let _cwd_guard = CwdGuard::set(&project_dir);
    let cfg = resolve_config(None).await.unwrap();

    assert_eq!(cfg.llm.len(), 1);
    assert_eq!(cfg.llm[0].provider, "anthropic");
    assert_eq!(cfg.llm[0].api_key, "project-key");

    let warnings = take_fallback_warnings();
    assert!(warnings.is_empty());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_path_same_as_user_config_no_duplicate() {
    let _guard = fs_lock();
    clear_fallback_warnings();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let user_path = home.join(".config").join("review-engine");
    std::fs::create_dir_all(&user_path).unwrap();
    let user_config = user_path.join(".code-audit-config.toml");
    std::fs::write(&user_config, user_llm_toml()).unwrap();

    let _home_guard = HomeGuard::set(&home);
    let cfg = resolve_config(Some(ConfigSource::Path(user_config.to_string_lossy().to_string())))
        .await
        .unwrap();

    assert_eq!(cfg.llm.len(), 1);
    assert_eq!(cfg.llm[0].api_key, "user-key");

    let warnings = take_fallback_warnings();
    assert!(warnings.is_empty());
}

// ── [report] section tests (None path) ──────────────────────────────────

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_none_user_report_applied() {
    let _guard = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let user_path = home.join(".config").join("review-engine");
    std::fs::create_dir_all(&user_path).unwrap();
    std::fs::write(
        user_path.join(".code-audit-config.toml"),
        "[report]\nverification_pass = true\naggregated = true\n",
    )
    .unwrap();

    // No project-level config file at all.
    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let _home_guard = HomeGuard::set(&home);
    let _cwd_guard = CwdGuard::set(&project_dir);
    let cfg = resolve_config(None).await.unwrap();

    assert!(cfg.report.verification_pass);
    assert!(cfg.report.aggregated);
    // Fields omitted from the user-level [report] keep serde defaults.
    assert_eq!(cfg.report.max_findings_per_expert, 5);
    assert_eq!(cfg.report.verification_max_file_bytes, 20000);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_none_project_report_overrides_user() {
    let _guard = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let user_path = home.join(".config").join("review-engine");
    std::fs::create_dir_all(&user_path).unwrap();
    std::fs::write(
        user_path.join(".code-audit-config.toml"),
        "[report]\nverification_pass = false\naggregated = true\n",
    )
    .unwrap();

    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join(".code-audit-config.toml"),
        "[report]\nverification_pass = true\n",
    )
    .unwrap();

    let _home_guard = HomeGuard::set(&home);
    let _cwd_guard = CwdGuard::set(&project_dir);
    let cfg = resolve_config(None).await.unwrap();

    // Project-level [report] wins over the user-level one.
    assert!(cfg.report.verification_pass);
    // Wholesale-replacement semantics: `aggregated` is NOT inherited from
    // the user-level config; it falls back to the serde default (false).
    assert!(!cfg.report.aggregated);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_none_project_report_disables_user_setting() {
    let _guard = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let user_path = home.join(".config").join("review-engine");
    std::fs::create_dir_all(&user_path).unwrap();
    std::fs::write(
        user_path.join(".code-audit-config.toml"),
        "[report]\nverification_pass = true\n",
    )
    .unwrap();

    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join(".code-audit-config.toml"),
        "[report]\nverification_pass = false\n",
    )
    .unwrap();

    let _home_guard = HomeGuard::set(&home);
    let _cwd_guard = CwdGuard::set(&project_dir);
    let cfg = resolve_config(None).await.unwrap();

    // An explicit project-level `false` overrides the user-level `true`.
    assert!(!cfg.report.verification_pass);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_none_no_report_keeps_builtin_default() {
    let _guard = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let user_path = home.join(".config").join("review-engine");
    std::fs::create_dir_all(&user_path).unwrap();
    // User-level config without a [report] section.
    std::fs::write(user_path.join(".code-audit-config.toml"), user_llm_toml()).unwrap();

    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    // Project-level config without a [report] section.
    std::fs::write(
        project_dir.join(".code-audit-config.toml"),
        "[commands]\nreview = true\n",
    )
    .unwrap();

    let _home_guard = HomeGuard::set(&home);
    let _cwd_guard = CwdGuard::set(&project_dir);
    let cfg = resolve_config(None).await.unwrap();

    assert!(!cfg.report.verification_pass);
    assert!(!cfg.report.aggregated);
    assert_eq!(cfg.report.max_findings_per_expert, 5);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_resolve_config_none_invalid_user_toml_keeps_default_report() {
    let _guard = fs_lock();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let user_path = home.join(".config").join("review-engine");
    std::fs::create_dir_all(&user_path).unwrap();
    // Malformed user-level TOML: must not panic, falls back to defaults.
    std::fs::write(user_path.join(".code-audit-config.toml"), "this is = not [ valid toml").unwrap();

    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let _home_guard = HomeGuard::set(&home);
    let _cwd_guard = CwdGuard::set(&project_dir);
    let cfg = resolve_config(None).await.unwrap();

    assert!(!cfg.report.verification_pass);
    assert!(!cfg.report.aggregated);
    assert!(cfg.llm.is_empty());
}
