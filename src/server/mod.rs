//! HTTP server with REST API, webhooks, and task queue.
//!
//! Built on Axum, this module exposes a web server that serves the
//! review-engine REST API (routes under `api/`), handles incoming
//! webhooks from GitLab and GitHub (via the `gitlab`, `github`, and
//! provider-agnostic `webhook` submodules), manages review
//! authentication via `auth`, and provides a background task queue
//! (`task_queue`) for asynchronous review processing. Application state
//! is defined in [`state`], and the Axum [`Router`] is constructed by the
//! [`router`] submodule.

use std::sync::Arc;

pub mod api;
pub mod auth;
pub mod dispatcher;
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
    } else if let Ok(json) = std::env::var("LLM_CONFIG") {
        serde_json::from_str(&json)?
    } else {
        Vec::new()
    };

    // Select experts for the review command
    let experts = config.build_expert_defs();

    // Run the review with progress tracking
    let progress_map = crate::progress::new_progress_map();
    let review_id = uuid::Uuid::new_v4().to_string();
    let (reports, global_context) = orchestrator::run_experts(
        &experts,
        &mr_info,
        &diff,
        &llm_configs,
        &config,
        Some(progress_map.clone()),
        &review_id,
    )
    .await?;

    // Run aggregator if available
    if let Some(aggregator) = experts.iter().find(|e| e.name == "aggregator") {
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
            Ok(agg) => tracing::info!("Aggregator completed: {} findings", agg.findings.len()),
            Err(e) => tracing::warn!("Aggregator failed: {:?}", e),
        }
    }

    // Mark progress complete
    crate::progress::complete_progress(Some(&progress_map), &review_id);

    // Publish results
    let output = crate::models::ReviewOutput::new(reports);
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
