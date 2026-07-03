//! Configuration resolution, environment overrides, and validation.
//!
//! Provides [`resolve_config`] for auto-detecting and loading configuration
//! from files or inline sources, [`apply_env_overrides`] for environment
//! variable overrides, and [`validate_experts`] for expert weight validation.

use crate::config::defaults::{default_config, merge_default, parse_toml};
use crate::models::*;
use anyhow::Result;
use std::collections::HashMap;

/// Parse, merge, apply environment overrides, and validate a TOML config string.
pub fn load_and_apply(toml_content: &str) -> Result<AppConfig> {
    let parsed = parse_toml(toml_content)?;
    let merged = merge_default(parsed)?;
    let config = apply_env_overrides(merged);
    validate_experts(&config)?;
    Ok(config)
}

fn apply_env_overrides(mut config: AppConfig) -> AppConfig {
    if let Ok(val) = std::env::var("CODE_AUDIT_COMMANDS") {
        match toml::from_str::<HashMap<String, bool>>(&val) {
            Ok(parsed) => {
                for (k, v) in parsed {
                    config.commands.insert(k, v);
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "CODE_AUDIT_COMMANDS is set but could not be parsed as a boolean map; ignoring"
                );
            }
        }
    }
    if let Ok(val) = std::env::var("CODE_AUDIT_SCORING_ENABLED") {
        config.scoring.enabled = val == "true" || val == "1";
    }
    config
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
    let mut wrapper = toml::map::Map::new();
    wrapper.insert("llm".to_string(), val.clone());
    match toml::from_str::<crate::models::AppConfig>(&toml::Value::Table(wrapper).to_string()) {
        Ok(cfg) => cfg.llm,
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

/// Resolve the application configuration from the given source (or auto-detect).
///
/// Resolution order (three-level merge):
/// 1. Built-in defaults + environment-variable overrides (base).
/// 2. `~/.config/review-engine/.code-audit-config.toml` — fills `[[llm]]` and
///    other fields that the project config doesn't set.
/// 3. `.code-audit-config.toml` in the current directory — overrides everything
///    above, except `[[llm]]` which is only overridden if the project config
///    explicitly provides it.
pub async fn resolve_config(source: Option<ConfigSource>) -> Result<AppConfig> {
    match source {
        Some(ConfigSource::Inline(toml_str)) => load_and_apply(&toml_str),
        Some(ConfigSource::Path(path)) => {
            if !std::path::Path::new(&path).exists() {
                anyhow::bail!("config file not found: {}", path);
            }
            let content = tokio::fs::read_to_string(&path).await?;
            load_and_apply(&content)
        }
        None => {
            let default_path = ".code-audit-config.toml";
            let user_config_path =
                home::home_dir().map(|p| p.join(".config").join("review-engine").join(".code-audit-config.toml"));

            // 1. Built-in defaults + env overrides
            let mut config = apply_env_overrides(default_config()?);

            // 2. User-level config (~/.config/review-engine/) — fills gaps
            if let Some(ref user_path) = user_config_path {
                if user_path.exists() {
                    match std::fs::read_to_string(user_path) {
                        Ok(content) => {
                            match toml::from_str::<toml::Value>(&content) {
                                Ok(val) => {
                                    if let Some(obj) = val.as_table() {
                                        // LLM: fill only if base has none
                                        if config.llm.is_empty() {
                                            if let Some(llm) = obj.get("llm") {
                                                let parsed = take_llm(llm);
                                                if !parsed.is_empty() {
                                                    config.llm = parsed;
                                                }
                                            }
                                        }
                                        // Commands: merge
                                        if let Some(cmds) = obj.get("commands") {
                                            config.commands.extend(take_commands(cmds));
                                        }
                                        // Experts: fill (don't override project)
                                        if let Some(review_experts) = obj.get("review_experts") {
                                            match toml::from_str::<HashMap<String, crate::models::ExpertTomlDef>>(
                                                &review_experts.to_string(),
                                            ) {
                                                Ok(parsed) => {
                                                    for (k, v) in parsed {
                                                        config.review_experts.entry(k).or_insert(v);
                                                    }
                                                }
                                                Err(e) => {
                                                    tracing::warn!(
                                                        path = %user_path.display(),
                                                        error = %e,
                                                        "Failed to parse user-level review_experts section; ignoring"
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        path = %user_path.display(),
                                        error = %e,
                                        "Failed to parse user-level config file as TOML; ignoring"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                path = %user_path.display(),
                                error = %e,
                                "Failed to read user-level config file; ignoring"
                            );
                        }
                    }
                }
            }

            // 3. Project-level config (.code-audit-config.toml) — overrides
            if std::path::Path::new(default_path).exists() {
                match tokio::fs::read_to_string(default_path).await {
                    Ok(content) => {
                        match toml::from_str::<toml::Value>(&content) {
                            Ok(val) => {
                                if let Some(obj) = val.as_table() {
                                    // LLM: override only if project explicitly provides [[llm]]
                                    if let Some(llm) = obj.get("llm") {
                                        let parsed = take_llm(llm);
                                        if !parsed.is_empty() {
                                            config.llm = parsed;
                                        }
                                    }
                                    // Commands: override
                                    if let Some(cmds) = obj.get("commands") {
                                        config.commands.extend(take_commands(cmds));
                                    }
                                    // Experts: override (project wins over user)
                                    if let Some(review_experts) = obj.get("review_experts") {
                                        match toml::from_str::<HashMap<String, crate::models::ExpertTomlDef>>(
                                            &review_experts.to_string(),
                                        ) {
                                            Ok(parsed) => {
                                                config.review_experts.extend(parsed);
                                            }
                                            Err(e) => {
                                                tracing::warn!(
                                                    path = default_path,
                                                    error = %e,
                                                    "Failed to parse project-level review_experts section; ignoring"
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    path = default_path,
                                    error = %e,
                                    "Failed to parse project-level config file as TOML; ignoring"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = default_path,
                            error = %e,
                            "Failed to read project-level config file; ignoring"
                        );
                    }
                }
            }

            Ok(config)
        }
    }
}

/// A config resolver that wraps [`resolve_config`] for dependency injection.
pub struct ConfigResolver;

impl ConfigResolver {
    /// Create a new `ConfigResolver`.
    pub fn new() -> Self {
        Self
    }

    /// Resolve the application configuration from the given source.
    ///
    /// Delegates to [`resolve_config`]; see its documentation for resolution order.
    pub async fn resolve(&self, source: Option<ConfigSource>) -> Result<AppConfig> {
        resolve_config(source).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

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

    #[test]
    fn test_apply_env_overrides_invalid_toml() {
        let _guard = ENV_LOCK.lock().unwrap();
        let env_guard = EnvGuard::new("CODE_AUDIT_COMMANDS");
        env_guard.set("not valid toml {{{");

        let config = default_config().unwrap();
        let overridden = apply_env_overrides(config);
        // Invalid TOML should be silently ignored, commands unchanged
        assert_eq!(overridden.commands.len(), 6);
    }

    #[test]
    fn test_apply_env_overrides_scoring_non_bool() {
        let _guard = ENV_LOCK.lock().unwrap();
        let env_guard = EnvGuard::new("CODE_AUDIT_SCORING_ENABLED");
        env_guard.set("not_a_boolean");

        let config = default_config().unwrap();
        let overridden = apply_env_overrides(config);
        // Non-boolean should be treated as false
        assert!(!overridden.scoring.enabled);
    }

    #[test]
    fn test_apply_env_overrides_commands() {
        let _guard = ENV_LOCK.lock().unwrap();
        let env_guard = EnvGuard::new("CODE_AUDIT_COMMANDS");
        env_guard.set("review = true\ndescribe = true");

        let config = default_config().unwrap();
        assert!(!*config.commands.get("review").unwrap());

        let overridden = apply_env_overrides(config);
        assert!(*overridden.commands.get("review").unwrap());
        assert!(*overridden.commands.get("describe").unwrap());
    }

    #[test]
    fn test_apply_env_overrides_empty_commands() {
        let _guard = ENV_LOCK.lock().unwrap();
        let env_guard = EnvGuard::new("CODE_AUDIT_COMMANDS");
        env_guard.set("");

        let config = default_config().unwrap();
        let overridden = apply_env_overrides(config);
        // Empty string is invalid TOML and should be silently ignored
        assert_eq!(overridden.commands.len(), 6);
    }

    #[test]
    fn test_validate_experts_weight_sum_not_100() {
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
        // Explicit config — disable all defaults, use only alice+bob = 100
        let user_toml = format!("{}[review_experts.alice]\nenabled = true\nweight = 60\n\n[review_experts.bob]\nenabled = true\nweight = 40\n", base_disabled_toml());
        let cfg = load_and_apply(&user_toml).unwrap();
        validate_experts(&cfg).unwrap();
    }

    #[test]
    fn test_validate_experts_no_enabled_experts() {
        let user_toml = base_disabled_toml();
        let cfg = load_and_apply(&user_toml).unwrap();
        validate_experts(&cfg).unwrap();
    }

    #[test]
    fn test_validate_experts_zero_weight_sum() {
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
    async fn test_resolve_config_default_path() {
        let cfg = resolve_config(None).await.unwrap();
        assert!(cfg.review_experts.contains_key("lead"));
    }

    #[tokio::test]
    async fn test_resolve_config_inline() {
        let cfg = resolve_config(Some(ConfigSource::Inline("[commands]\nreview = true".to_string())))
            .await
            .unwrap();
        assert!(*cfg.commands.get("review").unwrap());
    }

    #[tokio::test]
    async fn test_resolve_config_with_invalid_weight_sum() {
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
    async fn test_resolve_config_inline_full_toml() {
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
    async fn test_resolve_config_inline_empty_still_defaults() {
        let cfg = resolve_config(Some(ConfigSource::Inline(String::new()))).await.unwrap();
        // With empty inline, should parse with all defaults
        assert!(cfg.review_experts.contains_key("lead"));
        assert_eq!(cfg.diff.max_input_tokens, 120000);
    }

    #[test]
    fn test_apply_env_overrides_enables_scoring() {
        let _guard = ENV_LOCK.lock().unwrap();
        let env_guard = EnvGuard::new("CODE_AUDIT_SCORING_ENABLED");
        env_guard.set("true");

        let config = default_config().unwrap();
        let overridden = apply_env_overrides(config);
        assert!(overridden.scoring.enabled);
    }

    #[test]
    fn test_apply_env_overrides_disables_scoring_with_0() {
        let _guard = ENV_LOCK.lock().unwrap();
        let env_guard = EnvGuard::new("CODE_AUDIT_SCORING_ENABLED");
        env_guard.set("0");

        let config = default_config().unwrap();
        let overridden = apply_env_overrides(config);
        assert!(!overridden.scoring.enabled);
    }

    #[test]
    fn test_apply_env_overrides_scoring_with_1() {
        let _guard = ENV_LOCK.lock().unwrap();
        let env_guard = EnvGuard::new("CODE_AUDIT_SCORING_ENABLED");
        env_guard.set("1");

        let config = default_config().unwrap();
        let overridden = apply_env_overrides(config);
        assert!(overridden.scoring.enabled);
    }

    #[test]
    fn test_apply_env_overrides_unset_does_nothing() {
        let _guard = ENV_LOCK.lock().unwrap();
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
        assert!(!*overridden.commands.get("review").unwrap());
    }

    #[test]
    fn test_validate_experts_returns_ok_for_sum_100() {
        let user_toml = format!(
            "{}[review_experts.alice]\nenabled = true\nweight = 50\n\n[review_experts.bob]\nenabled = true\nweight = 50\n",
            base_disabled_toml()
        );
        let cfg = load_and_apply(&user_toml).unwrap();
        assert!(validate_experts(&cfg).is_ok());
    }

    #[test]
    fn test_validate_experts_err_for_sum_not_100() {
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
    async fn test_resolve_config_inline_with_commands() {
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
    async fn test_resolve_config_inline_with_diff_config() {
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
        let _guard = ENV_LOCK.lock().unwrap();
        let _env_guard = EnvGuard::new("CODE_AUDIT_SCORING_ENABLED");
        std::env::remove_var("CODE_AUDIT_SCORING_ENABLED");

        let cfg = resolve_config(Some(ConfigSource::Inline(toml.to_string())))
            .await
            .unwrap();
        assert!(!cfg.scoring.enabled);
        assert!(!cfg.scoring.display_individual_scores);
    }

    #[test]
    fn test_load_and_apply_empty_toml() {
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
        let _guard = ENV_LOCK.lock().unwrap();
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
        let user_toml = format!(
            "{}[review_experts.solo]\nenabled = true\nweight = 100\n",
            base_disabled_toml()
        );
        let cfg = load_and_apply(&user_toml).unwrap();
        assert!(validate_experts(&cfg).is_ok());
    }

    #[test]
    fn test_validate_experts_invalid_config_missing_fields() {
        // Overriding lead weight to 100 without disabling other default experts
        // causes the enabled experts' weights to sum to more than 100.
        let user_toml = r#"
[review_experts.lead]
enabled = true
weight = 100
"#;
        let result = load_and_apply(user_toml);
        assert!(result.is_err());
    }
}
