//! Application state shared across all HTTP route handlers.
//!
//! [`AppState`] is injected into every Axum route via
//! `axum::extract::State`. It holds LLM configurations, the Prometheus
//! metrics registry, review progress tracking, the background task
//! store, and the resolved application configuration.

use prometheus::Registry;
use std::sync::Arc;

use crate::models::LLMConfig;
use crate::server::task_queue::TaskStore;

/// Shared application state injected into every Axum route handler.
pub struct AppState {
    /// LLM configurations available for review prompts.
    pub llm_configs: Vec<LLMConfig>,
    /// Prometheus metrics registry (optional).
    pub registry: Option<Registry>,
    /// Shared progress map for tracking review status (optional).
    pub progress_map: Option<crate::progress::ProgressMap>,
    /// Background task store for async review processing (optional).
    pub task_store: Option<Arc<TaskStore>>,
    /// Resolved application configuration (optional).
    pub app_config: Option<Arc<crate::models::AppConfig>>,
}

impl AppState {
    /// Create a new `AppState` with the given LLM configs.
    ///
    /// All optional fields are initialised to `None`; set them directly
    /// or with builder-style methods as needed.
    pub fn new(llm_configs: Vec<LLMConfig>) -> Self {
        Self {
            llm_configs,
            registry: None,
            progress_map: None,
            task_store: None,
            app_config: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_state_new_empty() {
        let state = AppState::new(vec![]);
        assert!(state.llm_configs.is_empty());
        assert!(state.registry.is_none());
        assert!(state.progress_map.is_none());
        assert!(state.task_store.is_none());
        assert!(state.app_config.is_none());
    }

    #[test]
    fn test_app_state_new_with_configs() {
        let configs = vec![LLMConfig {
            provider: "openai".to_string(),
            model: "gpt-4".to_string(),
            api_key: "sk-test".to_string(),
            api_base: String::new(),
            max_tokens: 4096,
            temperature: 0.7,
        }];
        let state = AppState::new(configs);
        assert_eq!(state.llm_configs.len(), 1);
        assert_eq!(state.llm_configs[0].provider, "openai");
        assert!(state.registry.is_none());
    }

    #[test]
    fn test_app_state_fields_are_pub() {
        // Verify that fields are accessible (they're pub)
        let state = AppState::new(vec![]);
        let _llm: &Vec<LLMConfig> = &state.llm_configs;
        let _reg: &Option<Registry> = &state.registry;
    }
}
