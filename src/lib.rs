//! Code review engine — an AI-powered, multi-expert review orchestrator.
//!
//! This module is part of the review-engine CodeReview Board platform.
//!
//! This library provides a complete pipeline for automated code review:
//! it accepts input from local Git repositories or remote Git providers
//! (GitLab, GitHub), parses diffs, dispatches reviews to a virtual team
//! of LLM and static experts, scores findings, and publishes results
//! back as MR/PR discussions or to local output files. The architecture
//! is modular, with clear trait boundaries for providers, experts,
//! and orchestrators, making it extensible to new platforms
//! and review strategies.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod actions;
pub mod config;
pub mod diff;
pub mod error;
pub mod expert;
pub mod git;
pub mod git_provider;
pub mod input;
pub mod language;
pub mod llm;
/// Prometheus metrics used by the server, CLI, and LLM client.
pub mod metrics;
pub mod models;
pub mod output;
pub mod progress;
pub mod prompt;
pub mod publisher;
pub mod repo;
pub mod review_context;
pub mod scoring;
pub mod server;
pub mod team;
pub mod tokenizer;

/// Optional PyO3 bindings for calling review-engine from Python.
/// Only compiled when the `python` feature is enabled.
#[cfg(feature = "python")]
pub mod python;

use anyhow::{Context, Result};
pub use models::AppConfig;
use models::*;

/// Run a full multi-expert review pipeline for a GitLab MR.
///
/// Fetches the MR info and diff from GitLab, resolves configuration,
/// builds expert definitions, runs all applicable experts (optionally
/// including an aggregator expert), and returns the combined
/// [`ReviewOutput`] with per-expert reports.
///
/// # Arguments
/// * `mr_url` - Full URL to the GitLab merge request.
/// * `gitlab_token` - GitLab personal access token for API access.
/// * `llm_configs` - LLM provider configurations for AI-powered experts.
/// * `config_source` - Optional config source (inline, path, or auto-detect).
/// * `progress_override` - Optional progress map and review ID for tracking.
pub async fn run_review(
    mr_url: &str,
    gitlab_token: &str,
    llm_configs: Vec<LLMConfig>,
    config_source: Option<ConfigSource>,
    progress_override: Option<(crate::progress::ProgressMap, String)>,
) -> Result<ReviewOutput> {
    let config = config::resolve_config(config_source.clone()).await?;

    let gitlab_client = git_provider::gitlab::client::Client::new(gitlab_token, mr_url)?;
    let mr_info = gitlab_client.fetch_mr_info().await?;
    let diff = gitlab_client.fetch_diff().await?;

    let app_config = match config_source {
        Some(ConfigSource::Inline(_)) => config.clone(),
        Some(ConfigSource::Path(_)) => config.clone(),
        None => match gitlab_client.fetch_config_toml().await {
            Ok(Some(toml_content)) => config::merge_default(config::parse_toml(&toml_content)?),
            Ok(None) => Ok(config),
            Err(_) => Ok(config),
        }?,
    };

    let experts = app_config.build_expert_defs();

    let aggregated = app_config.report.aggregated && experts.iter().any(|e| e.name == "aggregator");

    let (progress_map, review_id) = match progress_override {
        Some((map, id)) => (map, id),
        None => (crate::progress::new_progress_map(), uuid::Uuid::new_v4().to_string()),
    };

    let (findings, global_context) = crate::team::orchestrator::run_experts(
        &experts,
        &mr_info,
        &diff,
        &llm_configs,
        &app_config,
        Some(progress_map.clone()),
        &review_id,
    )
    .await?;

    let output = if aggregated {
        match experts.iter().find(|e| e.name == "aggregator") {
            Some(aggregator) => {
                let aggregated_report = crate::team::orchestrator::run_aggregator(
                    aggregator,
                    &findings,
                    &llm_configs,
                    &mr_info,
                    global_context.as_ref(),
                    Some(progress_map.clone()),
                    &review_id,
                )
                .await?;
                ReviewOutput::with_aggregated(findings, aggregated_report)
            }
            None => ReviewOutput::new(findings),
        }
    } else {
        ReviewOutput::new(findings)
    };

    // Mark progress complete
    crate::progress::complete_progress(Some(&progress_map), &review_id);

    Ok(output)
}

/// Publish review results back to an MR/PR discussion.
///
/// Automatically selects the right Git provider based on the MR URL:
/// - `github.com` → `GitHubProvider`
/// - everything else → `GitLabProvider`
///
/// On failure, only logs a warning — does not return an error,
/// since the review itself has already completed successfully.
pub async fn publish_review(token: &str, mr_url: &str, output: &ReviewOutput) -> Result<()> {
    let provider: Box<dyn crate::git_provider::GitProvider> =
        if mr_url.contains(".github.") || mr_url.contains("github.com") {
            crate::git_provider::github::GitHubProvider::new(token, mr_url)
                .map(|p| Box::new(p) as Box<dyn crate::git_provider::GitProvider>)
                .context("Failed to create GitHubProvider")?
        } else {
            crate::git_provider::gitlab::GitLabProvider::new(token, mr_url)
                .map(|p| Box::new(p) as Box<dyn crate::git_provider::GitProvider>)
                .context("Failed to create GitLabProvider")?
        };

    let mut md = String::from("# CodeReview Board\n\n");
    for report in &output.reports {
        md.push_str(&report.markdown);
        md.push_str("\n\n---\n\n");
    }

    let mut errors: Vec<anyhow::Error> = Vec::new();

    if let Err(e) = provider.find_or_update_discussion(&md).await {
        errors.push(e.context("discussion"));
    }

    for report in &output.reports {
        if let Err(e) = crate::publisher::publish_inline_notes(&*provider, &report.findings).await {
            errors.push(e.context("inline notes"));
        }
    }

    match errors.len() {
        0 => Ok(()),
        1 => Err(errors.swap_remove(0)),
        _ => {
            let first = errors.swap_remove(0);
            Err(errors.into_iter().fold(first, |acc, e| acc.context(e)))
        }
    }
}
