//! HTTP server with REST API, webhooks, and task queue.
//!
//! Built on Axum, this module exposes a web server that serves the
//! review-engine REST API (routes under `api/`), handles incoming
//! webhooks from GitLab and GitHub (via the `gitlab`, `github`, and
//! provider-agnostic `webhook` submodules), manages review
//! authentication via `auth`, provides a background task queue
//! (`task_queue`) for asynchronous review processing, and persists
//! finding feedback via `feedback`. Application state
//! is defined in [`state`], and the Axum [`Router`] is constructed by the
//! [`router`] submodule.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

pub mod api;
pub mod auth;
pub mod dispatcher;
pub mod feedback;
pub mod github;
pub mod gitlab;
pub mod log_collector;
pub mod router;
pub mod routes;
pub mod state;
pub mod task_queue;
pub mod webhook;

pub use state::AppState;

use self::auth::AuthConfig;
use self::dispatcher::MrDispatcher;

use crate::git_provider::GitProvider;

// ─── Aggregator 决策 + 输出构造（纯函数，可单测）────────────

/// Decide whether to run the aggregator expert.
///
/// Returns `Some(aggregator)` when aggregation is enabled _and_ an expert
/// named `"aggregator"` exists in the active team. Otherwise returns `None`.
///
/// This function contains no network I/O and is fully deterministic given its inputs,
/// making it ideal for unit testing the four config × presence combinations.
pub(crate) fn select_aggregator_expert(
    aggregated: bool,
    experts: &[crate::models::ExpertDef],
) -> Option<&crate::models::ExpertDef> {
    if !aggregated {
        return None;
    }
    experts.iter().find(|e| e.name == "aggregator")
}

/// Build [`ReviewOutput`](crate::models::ReviewOutput) from expert reports and an optional aggregator result.
///
/// - `Some(report)` → wrapped with aggregation via `with_aggregated`.
/// - `None` → plain output via `new`, **fail-soft** (error is NOT propagated).
///
/// Callers that wish to log the error should do so before invoking this function.
pub(crate) fn build_review_output_from_reports(
    reports: Vec<crate::models::ExpertReport>,
    aggregated: Option<crate::models::AggregatedReport>,
) -> crate::models::ReviewOutput {
    match aggregated {
        Some(report) => crate::models::ReviewOutput::with_aggregated(reports, report),
        None => crate::models::ReviewOutput::new(reports),
    }
}

/// Shared review execution logic used by both GitLab and GitHub webhook handlers.
///
/// Creates the appropriate provider from the URL, fetches the MR/PR info and diff,
/// runs the expert team, optionally runs the aggregator, publishes results, and
/// notifies the dispatcher upon completion.
pub(crate) async fn run_review_common(
    url: &str,
    token: &str,
    dispatcher: Option<&MrDispatcher>,
    dispatch_key: Option<&str>,
    sha: Option<&str>,
) -> anyhow::Result<()> {
    use crate::config;
    use crate::team::orchestrator;

    let config = config::resolve_config(None).await?;

    // Determine provider type from URL
    let provider: Box<dyn GitProvider> = if url.contains("github.com") || url.contains(".github.") {
        Box::new(crate::git_provider::github::GitHubProvider::new(token, url)?)
    } else {
        Box::new(crate::git_provider::gitlab::GitLabProvider::new(token, url)?)
    };

    let mr_info = provider.fetch_mr_info().await?;
    let diff = provider.fetch_diff().await?;

    if diff.is_empty() {
        tracing::info!("No diff changes, skipping review");
        if let (Some(d), Some(key), Some(s)) = (dispatcher, dispatch_key, sha) {
            d.complete(key, s).await;
        }
        return Ok(());
    }

    // Set up LLM configs
    let llm_configs: Vec<crate::models::LLMConfig> = if !config.llm.is_empty() {
        config.llm.clone()
    } else {
        crate::config::llm_configs_from_env()
    };

    // Select experts for the review command
    let experts = config.build_expert_defs();

    // Run the review with progress tracking
    let progress_map = crate::progress::new_progress_map();
    let review_id = uuid::Uuid::new_v4().to_string();
    let (reports, global_context, dropped_findings, consolidated) = orchestrator::run_experts(
        &experts,
        &mr_info,
        &diff,
        &llm_configs,
        &config,
        Some(progress_map.clone()),
        &review_id,
    )
    .await?;

    // Run aggregator if enabled in config and present in expert list
    let maybe_aggregated: Option<crate::models::AggregatedReport> =
        if let Some(aggregator) = select_aggregator_expert(config.report.aggregated, &experts) {
            match orchestrator::run_aggregator(
                aggregator,
                &reports,
                &llm_configs,
                &mr_info,
                global_context.as_ref(),
                Some(progress_map.clone()),
                &review_id,
            )
            .await
            {
                Ok(agg) => Some(agg),
                Err(e) => {
                    tracing::warn!(
                        "Failed to run aggregator: {:?}, falling back to non-aggregated output",
                        e
                    );
                    None
                }
            }
        } else {
            None
        };

    if let Some(ref agg) = maybe_aggregated {
        tracing::info!("Aggregator completed: {} findings", agg.findings.len());
    }

    // Build output from reports (and optional aggregator result)
    let output = build_review_output_from_reports(reports, maybe_aggregated);

    // Mark progress complete
    crate::progress::complete_progress(Some(&progress_map), &review_id);

    // Publish results
    let output = output
        .with_dropped_findings(dropped_findings)
        .with_consolidated(consolidated);
    if let Err(e) = crate::publish_review(token, url, &output).await {
        tracing::warn!("Publish failed: {:?}", e);
    }

    // Notify dispatcher that review is done
    if let (Some(d), Some(key), Some(s)) = (dispatcher, dispatch_key, sha) {
        d.complete(key, s).await;
    }

    // Log completion
    tracing::info!("Review completed for: {}", url);

    Ok(())
}

// ─── 单元测试 ────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AggregatedReport, ExpertDef, ExpertReport, ExpertTomlDef, Finding, Severity};

    // ─── select_aggregator_expert ─────────────

    #[test]
    fn when_aggregated_true_and_aggregator_exists_returns_some() {
        let security = ExpertDef::from((&String::from("security"), &ExpertTomlDef::default()));
        let aggregator = ExpertDef::from((&String::from("aggregator"), &ExpertTomlDef::default()));
        let experts = vec![security, aggregator];
        let result = select_aggregator_expert(true, &experts);
        assert!(
            result.is_some(),
            "expected Some when aggregated==true and aggregator expert exists"
        );
        assert_eq!(result.unwrap().name, "aggregator");
    }

    #[test]
    fn when_aggregated_true_but_no_aggregator_returns_none() {
        let experts = vec![ExpertDef::from((&String::from("security"), &ExpertTomlDef::default()))];
        assert!(
            select_aggregator_expert(true, &experts).is_none(),
            "expected None when aggregator expert is absent"
        );
    }

    #[test]
    fn when_aggregated_false_returns_none_even_with_aggregator() {
        let security = ExpertDef::from((&String::from("security"), &ExpertTomlDef::default()));
        let aggregator = ExpertDef::from((&String::from("aggregator"), &ExpertTomlDef::default()));
        let experts = vec![security, aggregator];
        assert!(
            select_aggregator_expert(false, &experts).is_none(),
            "expected None when aggregated is disabled"
        );
    }

    #[test]
    fn when_aggregated_false_and_no_experts_returns_none() {
        let experts: Vec<ExpertDef> = vec![];
        assert!(select_aggregator_expert(false, &experts).is_none());
    }

    #[test]
    fn when_aggregated_true_and_empty_experts_returns_none() {
        let experts: Vec<ExpertDef> = vec![];
        assert!(select_aggregator_expert(true, &experts).is_none());
    }

    // ─── build_review_output_from_reports ─────

    #[test]
    fn when_aggregated_some_wraps_with_aggregated() {
        let reports = vec![ExpertReport {
            expert_name: "security".to_string(),
            findings: vec![],
            markdown: String::new(),
            raw_llm_response: String::new(),
        }];
        let agg = Some(AggregatedReport {
            findings: vec![],
            markdown: String::new(),
            raw_llm_response: String::new(),
        });
        let output = build_review_output_from_reports(reports, agg);
        assert!(
            output.aggregated.is_some(),
            "output should carry aggregated when input is Some"
        );
    }

    #[test]
    fn when_aggregated_none_uses_plain_new() {
        let reports = vec![ExpertReport {
            expert_name: "security".to_string(),
            findings: vec![],
            markdown: String::new(),
            raw_llm_response: String::new(),
        }];
        let output = build_review_output_from_reports(reports, None);
        assert!(
            output.aggregated.is_none(),
            "output should be None when aggregator failed/is-disabled"
        );
    }

    // ─── 组合语义（完整 webhook 场景）─────────────

    #[test]
    fn full_flow_aggregator_enabled_and_successful_then_aggregated_is_some() {
        let security = ExpertDef::from((&String::from("security"), &ExpertTomlDef::default()));
        let aggregator = ExpertDef::from((&String::from("aggregator"), &ExpertTomlDef::default()));
        assert!(select_aggregator_expert(true, &[security, aggregator]).is_some());

        let agg_report = AggregatedReport {
            findings: vec![Finding {
                file: "src/main.rs".to_string(),
                line: Some(42),
                line_end: None,
                severity: Severity::High,
                confidence: 8,
                category: "security".to_string(),
                title: "Buffer overflow".to_string(),
                summary: String::new(),
                evidence: String::new(),
                impact: String::new(),
                recommendation: String::new(),
                effort: Default::default(),
                expert_name: "aggregator".to_string(),
                expert_role: String::new(),
                agrees_with: vec![],
                references: vec![],
            }],
            markdown: "# Summary\n".to_string(),
            raw_llm_response: "---\n".to_string(),
        };
        let reports = vec![ExpertReport {
            expert_name: "security".to_string(),
            findings: Default::default(),
            markdown: String::new(),
            raw_llm_response: String::new(),
        }];

        let output = build_review_output_from_reports(reports, Some(agg_report));
        assert!(output.aggregated.is_some());
        assert_eq!(output.aggregated.unwrap().findings.len(), 1);
    }

    #[test]
    fn full_flow_aggregator_disabled_then_aggregated_is_none() {
        let experts = vec![ExpertDef::from((&String::from("security"), &ExpertTomlDef::default()))];
        assert!(select_aggregator_expert(false, &experts).is_none());

        let reports = vec![ExpertReport {
            expert_name: "security".to_string(),
            findings: Default::default(),
            markdown: String::new(),
            raw_llm_response: String::new(),
        }];
        let output = build_review_output_from_reports(reports, None);
        assert!(output.aggregated.is_none());
    }

    #[test]
    fn full_flow_aggregator_not_in_team_then_aggregated_is_none() {
        let team = vec![
            ExpertDef::from((&String::from("architecture"), &ExpertTomlDef::default())),
            ExpertDef::from((&String::from("security"), &ExpertTomlDef::default())),
        ];
        assert!(select_aggregator_expert(true, &team).is_none());

        let reports = vec![ExpertReport {
            expert_name: "architecture".to_string(),
            findings: Default::default(),
            markdown: String::new(),
            raw_llm_response: String::new(),
        }];
        let output = build_review_output_from_reports(reports, None);
        assert!(output.aggregated.is_none());
    }
}

/// Start the health check and webhook server on the given port.
pub async fn serve(
    port: u16,
    bind: &str,
    state: Arc<AppState>,
    auth: Arc<AuthConfig>,
    webhook_handlers: Vec<Arc<dyn webhook::WebhookHandler>>,
) -> anyhow::Result<()> {
    let app = router::build(state, auth, webhook_handlers);

    let addr = format!("{}:{}", bind, port);
    tracing::info!("Health & webhook server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
