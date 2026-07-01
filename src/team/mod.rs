//! Virtual team review orchestration and lead consolidation.
//!
//! The [`TeamOrchestrator`] trait defines a five-stage review pipeline:
//! briefing, independent review, cross-check, lead consolidation, and
//! report delivery. The `orchestrator` submodule implements the full
//! pipeline, dispatching work to multiple LLM and static experts in
//! parallel. The `lead_consolidator` submodule merges and filters
//! expert reports into a single cohesive output. [`TeamReport`] and
//! [`ExpertMetrics`] capture the final results.

pub mod lead_consolidator;
pub mod orchestrator;

pub use crate::models::ExpertReport;

use crate::models::*;
use async_trait::async_trait;

/// The result of a full team review.
#[derive(Debug, Clone)]
pub struct TeamReport {
    pub reports: Vec<ExpertReport>,
    pub aggregated: Option<AggregatedReport>,
    pub team_size: usize,
    pub total_duration_ms: u64,
    pub total_tokens: u64,
    pub errors: Vec<String>,
    pub metrics: Vec<ExpertMetrics>,
    pub request_id: String,
}

/// Per-expert metrics collected during review.
#[derive(Debug, Clone)]
pub struct ExpertMetrics {
    pub name: String,
    pub latency_ms: u64,
    pub tokens_used: u64,
}

/// The core trait for orchestrating a virtual team review.
///
/// Follows a five-stage process:
/// 1. Briefing - Lead receives input, identifies risk areas
/// 2. Independent Review - Each expert reviews independently  
/// 3. Cross-check - Experts see each other's findings
/// 4. Lead Consolidation - Lead filters and merges
/// 5. Report Delivery - Final structured report
#[async_trait]
pub trait TeamOrchestrator: Send + Sync {
    /// Run a full team review for the given command and input.
    async fn run(
        &self,
        command: &Command,
        input: &ReviewInput,
        config: &AppConfig,
        llm_configs: &[LLMConfig],
    ) -> anyhow::Result<TeamReport>;

    /// Select experts for this command based on [commands] registry and expert config.
    fn select_experts<'a>(
        &self,
        command: &str,
        experts: &'a [ExpertDef],
        registry: &std::collections::HashMap<String, bool>,
    ) -> Vec<&'a ExpertDef>;
}
