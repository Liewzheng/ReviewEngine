use crate::models::AppConfig;

/// Sentinel masking a configured API key in `GET /config`. The frontend
/// renders it as "configured" and treats `""` or this sentinel as "leave
/// unchanged" on save (see `put_config`), so masking never destroys state.
pub const API_KEY_MASK: &str = "***";

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiConfig {
    #[serde(default)]
    pub gitlab: UiGitLabConfig,
    #[serde(default)]
    pub llm: UiLlmConfig,
    #[serde(default)]
    pub rules: UiRulesConfig,
    #[serde(default)]
    pub advanced: UiAdvancedConfig,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiGitLabConfig {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub api_token: String,
    #[serde(default)]
    pub webhook_secret: String,
    #[serde(default)]
    pub webhook_signing_secret: String,
    #[serde(default)]
    pub default_project: String,
    #[serde(default)]
    pub mr_label: String,
    #[serde(default)]
    pub auto_review: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiLlmProviderConfig {
    pub provider: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub api_base_url: String,
    #[serde(default)]
    pub default_model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u32,
    #[serde(default = "default_retry_attempts")]
    pub retry_attempts: u32,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiLlmConfig {
    #[serde(default)]
    pub primary_provider: String,
    #[serde(default)]
    pub openai_api_key: String,
    #[serde(default = "default_api_base_url")]
    pub api_base_url: String,
    #[serde(default)]
    pub default_model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u32,
    #[serde(default = "default_retry_attempts")]
    pub retry_attempts: u32,
    /// Multi-provider support — additive to the legacy single fields.
    #[serde(default)]
    pub providers: Vec<UiLlmProviderConfig>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiRulesConfig {
    #[serde(default = "default_min_score")]
    pub min_score: u32,
    #[serde(default)]
    pub block_on_critical: bool,
    #[serde(default)]
    pub auto_comment_on_pass: bool,
    #[serde(default = "default_comment_template")]
    pub comment_template: String,
    #[serde(default)]
    pub excluded_patterns: Vec<String>,
    #[serde(default = "default_required_experts")]
    pub required_experts: Vec<String>,
    #[serde(default = "default_max_review_duration_seconds")]
    pub max_review_duration_seconds: u32,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiAdvancedConfig {
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_log_retention_days")]
    pub log_retention_days: u32,
    #[serde(default = "default_sse_heartbeat_interval")]
    pub sse_heartbeat_interval: u32,
    #[serde(default = "default_max_concurrent_reviews")]
    pub max_concurrent_reviews: u32,
    #[serde(default = "default_request_timeout")]
    pub request_timeout: u32,
    #[serde(default = "default_enable_metrics")]
    pub enable_metrics: bool,
    #[serde(default)]
    pub debug_mode: bool,
}

pub(crate) fn default_max_tokens() -> u32 { 4096 }
pub(crate) fn default_api_base_url() -> String { "https://api.openai.com/v1".to_string() }
pub(crate) fn default_temperature() -> f32 { 0.7 }
pub(crate) fn default_timeout_seconds() -> u32 { 60 }
pub(crate) fn default_retry_attempts() -> u32 { 3 }
pub(crate) fn default_min_score() -> u32 { 75 }
pub(crate) fn default_comment_template() -> String {
    "Code review completed. Overall score: {{score}}/100. {{summary}}".to_string()
}
pub(crate) fn default_required_experts() -> Vec<String> {
    vec!["Security".to_string(), "Performance".to_string(), "Quality".to_string()]
}
pub(crate) fn default_max_review_duration_seconds() -> u32 { 300 }
pub(crate) fn default_log_level() -> String { "info".to_string() }
pub(crate) fn default_log_retention_days() -> u32 { 30 }
pub(crate) fn default_sse_heartbeat_interval() -> u32 { 15 }
pub(crate) fn default_max_concurrent_reviews() -> u32 { 5 }
pub(crate) fn default_request_timeout() -> u32 { 120 }
pub(crate) fn default_enable_metrics() -> bool { true }

impl UiConfig {
    /// Build a `UiConfig` from the backend-native `AppConfig`, filling in
    /// sensible defaults for fields that only exist in the UI layer.
    pub fn from_app_config(app: &AppConfig) -> Self {
        let mut ui = UiConfig::default();

        // Map LLM configs — legacy single fields
        for l in &app.llm {
            match l.provider.as_str() {
                "openai" => {
                    ui.llm.primary_provider = "openai".to_string();
                    ui.llm.openai_api_key = l.api_key.clone();
                    ui.llm.api_base_url = if l.api_base.is_empty() {
                        "https://api.openai.com/v1".to_string()
                    } else {
                        l.api_base.clone()
                    };
                    ui.llm.default_model = l.model.clone();
                    ui.llm.max_tokens = l.max_tokens;
                    ui.llm.temperature = l.temperature;
                }
                _ => {}
            }
        }
        // If primary_provider is still empty but we have at least one config
        if ui.llm.primary_provider.is_empty() {
            if let Some(first) = app.llm.first() {
                ui.llm.primary_provider = first.provider.clone();
                ui.llm.openai_api_key = first.api_key.clone();
                ui.llm.api_base_url = if first.api_base.is_empty() {
                    "https://api.openai.com/v1".to_string()
                } else {
                    first.api_base.clone()
                };
                ui.llm.default_model = first.model.clone();
                ui.llm.max_tokens = first.max_tokens;
                ui.llm.temperature = first.temperature;
            }
        }

        // Map all LLM configs as providers (multi-provider support)
        for l in &app.llm {
            ui.llm.providers.push(UiLlmProviderConfig {
                provider: l.provider.clone(),
                api_key: l.api_key.clone(),
                api_base_url: l.api_base.clone(),
                default_model: l.model.clone(),
                max_tokens: l.max_tokens,
                temperature: l.temperature,
                timeout_seconds: 60,
                retry_attempts: 3,
            });
        }

        // Map advanced settings
        ui.advanced.max_concurrent_reviews = app.max_concurrent_llm_calls.unwrap_or(5) as u32;
        ui.advanced.enable_metrics = true; // Default, overridden at runtime if needed

        // Apply defaults for fields not mapped from AppConfig
        if ui.llm.temperature == 0.0 {
            ui.llm.temperature = default_temperature();
        }
        if ui.rules.min_score == 0 {
            ui.rules.min_score = default_min_score();
        }

        ui
    }
}
