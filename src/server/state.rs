//! Application state shared across all HTTP route handlers.
//!
//! [`AppState`] is injected into every Axum route via
//! `axum::extract::State`. It holds LLM configurations, the Prometheus
//! metrics registry, review progress tracking, the background task
//! store, and the resolved application configuration.

use prometheus::Registry;
use std::sync::{Arc, Mutex, RwLock};

use crate::models::LLMConfig;
use crate::server::log_collector::LogCollector;
use crate::server::task_queue::TaskStore;

/// Shared application state injected into every Axum route handler.
pub struct AppState {
    /// LLM configurations available for review prompts (mutable for runtime updates).
    pub llm_configs: RwLock<Vec<LLMConfig>>,
    /// Prometheus metrics registry (optional).
    pub registry: Option<Registry>,
    /// Shared progress map for tracking review status (optional).
    pub progress_map: Option<crate::progress::ProgressMap>,
    /// Background task store for async review processing (optional).
    pub task_store: Option<Arc<TaskStore>>,
    /// Resolved application configuration (optional, wrapped for runtime mutation).
    pub app_config: RwLock<Option<Arc<crate::models::AppConfig>>>,
    /// In-memory log collector for SSE streaming (optional).
    pub log_collector: Option<Arc<Mutex<LogCollector>>>,
}

impl AppState {
    /// Create a new `AppState` with the given LLM configs.
    ///
    /// All optional fields are initialised to `None`; set them directly
    /// or with builder-style methods as needed.
    pub fn new(llm_configs: Vec<LLMConfig>) -> Self {
        Self {
            llm_configs: RwLock::new(llm_configs),
            registry: None,
            progress_map: None,
            task_store: None,
            app_config: RwLock::new(None),
            log_collector: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_state_new_empty() {
        let state = AppState::new(vec![]);
        assert!(state.llm_configs.read().unwrap().is_empty());
        assert!(state.registry.is_none());
        assert!(state.progress_map.is_none());
        assert!(state.task_store.is_none());
        assert!(state.app_config.read().unwrap().is_none());
        assert!(state.log_collector.is_none());
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
        let llm = state.llm_configs.read().unwrap();
        assert_eq!(llm.len(), 1);
        assert_eq!(llm[0].provider, "openai");
        assert!(state.registry.is_none());
    }

    #[test]
    fn test_app_state_fields_are_pub() {
        // Verify that fields are accessible (they're pub)
        let state = AppState::new(vec![]);
        let _llm: &RwLock<Vec<LLMConfig>> = &state.llm_configs;
        let _reg: &Option<Registry> = &state.registry;
    }
}
