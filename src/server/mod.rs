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
use self::dispatcher::{content_fingerprint, ContentDecision, ContentGate, MrDispatcher};

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

/// Resolve the MR/PR metadata and diff for a webhook-dispatched review.
///
/// Creates the appropriate provider from the URL and fetches the MR info and
/// diff in one place so callers can back-fill task source metadata before the
/// (possibly long) expert run starts.
pub(crate) async fn resolve_review_source(url: &str, token: &str) -> anyhow::Result<(crate::models::MRInfo, String)> {
    let provider: Box<dyn GitProvider> = if url.contains("github.com") || url.contains(".github.") {
        Box::new(crate::git_provider::github::GitHubProvider::new(token, url)?)
    } else {
        Box::new(crate::git_provider::gitlab::GitLabProvider::new(token, url)?)
    };

    let mr_info = provider.fetch_mr_info().await?;
    let diff = provider.fetch_diff().await?;
    Ok((mr_info, diff))
}

/// Resolve the LLM provider set for a webhook-dispatched review.
///
/// Precedence (highest first):
/// 1. **Config file `[[llm]]`** — the static, explicit configuration.
/// 2. **`server_llm_configs`** — the server's hot-applied providers
///    (`AppState::llm_configs`, populated from the DB `llm_providers` table at
///    startup and updated live by the WebUI / `PUT /api/v1/config`). An empty
///    vec counts as "not provided" (mirrors the 0.9.1 API-path semantics).
/// 3. **`LLM_CONFIG` env** — the Docker compose standard form.
///
/// `None` (CLI/legacy paths without an `AppState`) skips straight to the env
/// fallback, keeping pre-fix behavior exactly.
pub(crate) fn resolve_webhook_llm_configs(
    file_llm: &[crate::models::LLMConfig],
    server_llm_configs: Option<Vec<crate::models::LLMConfig>>,
) -> Vec<crate::models::LLMConfig> {
    if !file_llm.is_empty() {
        return file_llm.to_vec();
    }
    match server_llm_configs {
        Some(server) if !server.is_empty() => server,
        _ => crate::config::llm_configs_from_env(),
    }
}

/// Apply the server's persisted WebUI expert overrides to a freshly resolved
/// review config (RENG-69), in place — the experts counterpart of
/// [`resolve_webhook_llm_configs`].
///
/// The webhook review path resolves its config from the config file and never
/// reads `AppState::app_config`, so the overrides have to be replayed here:
/// **DB over file**, the config file staying the base/default (an override
/// patches an entry that exists, never adds one; unmentioned fields keep the
/// file value).
///
/// `None` (CLI/legacy paths without an `AppState`, tests) applies nothing —
/// pre-0.10.24 behaviour, where only the config file defined the team.
/// Returns the number of experts patched.
pub(crate) fn apply_server_expert_overrides(
    config: &mut crate::models::AppConfig,
    overrides: Option<&crate::config::ExpertOverrides>,
) -> usize {
    let Some(overrides) = overrides else {
        return 0;
    };
    let applied = overrides.apply_to(config);
    if applied > 0 {
        tracing::debug!(
            applied,
            "applied persisted expert overrides to the webhook review config"
        );
    }
    applied
}

/// Apply the unchanged-content gate (RENG-62) to a dispatch that already passed
/// [`MrDispatcher::try_start`].
///
/// The SHA guard alone cannot see an amend / force-push: the SHA changes while
/// the diff stays byte-identical, so the round would re-review (and re-post)
/// exactly what was already reviewed. `diff` is the diff this review would run
/// on — the caller has just fetched it, so the fingerprint costs no extra
/// provider call.
///
/// Returns the skip reason when the review must not run, `None` when it should
/// proceed (first sight, changed content, or [`ContentGate::Bypassed`] — an
/// explicit user-triggered run always reviews).
pub(crate) async fn gate_unchanged_content(
    dispatcher: &MrDispatcher,
    gate: ContentGate,
    mr_url: &str,
    sha: &str,
    diff: &str,
) -> Option<String> {
    if gate == ContentGate::Bypassed {
        return None;
    }
    match dispatcher.claim_content(mr_url, sha, &content_fingerprint(diff)).await {
        ContentDecision::Skip { reason } => Some(reason),
        ContentDecision::Review { reason } => {
            tracing::info!("Reviewing {mr_url}: {reason}");
            None
        }
    }
}

/// Shared review execution logic used by both GitLab and GitHub webhook handlers.
///
/// Runs the expert team against the already-resolved `mr_info` and `diff`, then:
///
/// 1. **Aggregator** — if `report.aggregated` is enabled _and_ an `"aggregator"` expert
///    exists in the team, runs it synchronously after all experts complete.
///    - **Success:** the aggregated report is merged into the [`ReviewOutput`](crate::models::ReviewOutput) and published alongside individual findings.
///    - **Failure:** fail-soft — logs a warning via `tracing::warn!`, sets aggregator output to `None`, _and_ continues with reports-only (all experts' findings are still published). The review itself is not aborted.
///
/// 2. **Output** — built by [`build_review_output_from_reports`](crate::server::build_review_output_from_reports) from expert reports and the optional aggregator result, then published via [`publish_review`].
///
/// Finally, notifies the dispatcher of completion — recording both the SHA and
/// the [`content_fingerprint`] of `diff`, the content the review actually
/// covered — and returns the constructed [`ReviewOutput`] (so the task store
/// can persist expert reports for the History detail panel).
///
/// `server_llm_configs` carries the server's hot-applied LLM providers
/// (`AppState::llm_configs`, snapshotted by the webhook handler at dispatch
/// time); see [`resolve_webhook_llm_configs`] for the precedence. `None` is
/// the CLI/legacy path (no server state) and keeps the pre-0.10.1
/// config-file → env behavior.
///
/// `llm_sink` (RENG-57) receives one latency sample per LLM call attempt this
/// review makes. The webhook handlers build it from the task store
/// ([`crate::server::task_queue::TaskStore::llm_sample_sink`]) once they have
/// created the task; a caller without a store passes `None`.
///
/// `server_expert_overrides` (RENG-69) is the matching snapshot of the
/// WebUI-managed expert edits: this function resolves its config from the
/// config file, so the overrides are re-applied over `[review_experts]` here.
/// `None` = no server state (CLI/tests) → the file values stand, exactly as
/// before 0.10.24.
pub(crate) async fn run_review_common(
    url: &str,
    token: &str,
    dispatcher: Option<&MrDispatcher>,
    dispatch_key: Option<&str>,
    sha: Option<&str>,
    mr_info: crate::models::MRInfo,
    diff: String,
    server_llm_configs: Option<Vec<crate::models::LLMConfig>>,
    llm_sink: Option<std::sync::Arc<dyn crate::llm::sampling::LlmCallSink>>,
    server_expert_overrides: Option<Arc<crate::config::ExpertOverrides>>,
) -> anyhow::Result<crate::models::ReviewOutput> {
    use crate::config;
    use crate::team::orchestrator;

    let mut config = config::resolve_config(None).await?;
    apply_server_expert_overrides(&mut config, server_expert_overrides.as_deref());

    if diff.is_empty() {
        tracing::info!("No diff changes, skipping review");
        if let (Some(d), Some(key), Some(s)) = (dispatcher, dispatch_key, sha) {
            d.complete(key, s, Some(&content_fingerprint(&diff))).await;
        }
        return Ok(crate::models::ReviewOutput::new(vec![]));
    }

    // Set up LLM configs: config file [[llm]] > server (WebUI/DB hot-applied)
    // > LLM_CONFIG env. Without the server fallback, providers configured in
    // the WebUI never reached webhook-triggered reviews (0.10.0 bug: the task
    // failed with "LLM config 'default' has no api_base set").
    let has_server_state = server_llm_configs.is_some();
    let llm_configs = resolve_webhook_llm_configs(&config.llm, server_llm_configs);
    if llm_configs.is_empty() {
        // Log-only; the failure itself keeps the pre-existing semantics. The
        // guidance matches the caller: with server state (real webhook
        // handlers) point at the WebUI; without it (legacy/test startup —
        // no WebUI exists) naming the WebUI would be misleading, so point at
        // the config file / env only.
        if has_server_state {
            tracing::error!(
                url = %url,
                "no LLM provider available for webhook-triggered review: the config file has no [[llm]] section, \
                 no provider is configured in the WebUI, and LLM_CONFIG is unset — add a provider via the WebUI \
                 (Configuration → LLM), the config file, or the LLM_CONFIG env var; the review will fail"
            );
        } else {
            tracing::error!(
                url = %url,
                "no LLM provider available for webhook-triggered review: the config file has no [[llm]] section \
                 and LLM_CONFIG is unset — add a provider via the config file or the LLM_CONFIG env var; \
                 the review will fail"
            );
        }
    }

    // Select experts for the review command
    let experts = config.build_expert_defs();

    // Run the review with progress tracking
    let progress_map = crate::progress::new_progress_map();
    let review_id = uuid::Uuid::new_v4().to_string();
    // RENG-31: this review fetched the diff through the provider API and has
    // no checkout, so the adjudication pass reads the cited files through the
    // provider at the reviewed SHA (token and SHA are both in hand here). The
    // webhook's own SHA wins when present; the MR metadata is the fallback.
    let revision = sha.filter(|s| !s.trim().is_empty()).unwrap_or(&mr_info.git_hash);
    let remote_files = crate::team::file_source::provider_source_or_warn(url, token, revision);
    let (reports, global_context, dropped_findings, consolidated, expert_failures) = orchestrator::run_experts(
        &experts,
        &mr_info,
        &diff,
        &llm_configs,
        &config,
        Some(progress_map.clone()),
        &review_id,
        None,
        remote_files,
        llm_sink.clone(),
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
                llm_sink,
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

    // Publish results. The diff the review actually reviewed is passed along so
    // inline notes are gated on its changed lines (no re-fetch, no anchors the
    // provider would reject).
    //
    // RENG-77 §4: the experts that produced no report are carried on the output
    // so a partially failed run is not silently published as a clean one.
    let output = output
        .with_dropped_findings(dropped_findings)
        .with_consolidated(consolidated)
        .with_errors(expert_failures);
    if !output.errors.is_empty() {
        tracing::warn!(
            failed = output.errors.len(),
            "{} expert(s) produced no report; the review is partial",
            output.errors.len()
        );
    }
    if let Err(e) = crate::publish_review_with_diff(token, url, &output, Some(&diff)).await {
        tracing::warn!("Publish failed: {:?}", e);
    }

    // Notify dispatcher that review is done, recording the fingerprint of the
    // diff this review actually covered (RENG-62) so an amend carrying the same
    // content is recognized as unchanged next time.
    if let (Some(d), Some(key), Some(s)) = (dispatcher, dispatch_key, sha) {
        d.complete(key, s, Some(&content_fingerprint(&diff))).await;
    }

    // Log completion
    tracing::info!("Review completed for: {}", url);

    Ok(output)
}

// ─── 单元测试 ────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AggregatedReport, ExpertDef, ExpertReport, ExpertTomlDef, Finding, Severity};

    // ─── resolve_webhook_llm_configs ──────────

    fn llm(provider: &str) -> crate::models::LLMConfig {
        crate::models::LLMConfig {
            provider: provider.to_string(),
            model: "m".to_string(),
            api_key: "k".to_string(),
            api_base: format!("http://{provider}.example"),
            max_tokens: 1024,
            temperature: 0.3,
            disable_thinking: None,
            disabled: false,
        }
    }

    #[test]
    fn file_llm_wins_over_server_and_env() {
        let resolved = resolve_webhook_llm_configs(&[llm("file")], Some(vec![llm("server")]));
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            resolved[0].provider, "file",
            "config file [[llm]] must win over server providers"
        );
    }

    #[test]
    fn server_llm_used_when_file_empty() {
        let resolved = resolve_webhook_llm_configs(&[], Some(vec![llm("server")]));
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            resolved[0].provider, "server",
            "empty config file must fall back to server providers"
        );
    }

    #[test]
    fn empty_server_vec_counts_as_not_provided() {
        // An explicit empty array is "not provided" (0.9.1 semantics): the
        // result must equal the env fallback, whatever the env holds.
        // (LLMConfig has no PartialEq — compare the provider sequence.)
        let resolved = resolve_webhook_llm_configs(&[], Some(vec![]));
        let env = crate::config::llm_configs_from_env();
        let providers: Vec<&str> = resolved.iter().map(|c| c.provider.as_str()).collect();
        let env_providers: Vec<&str> = env.iter().map(|c| c.provider.as_str()).collect();
        assert_eq!(
            providers, env_providers,
            "Some(vec![]) must fall through to the LLM_CONFIG env fallback"
        );
    }

    #[test]
    fn none_server_falls_back_to_env() {
        let resolved = resolve_webhook_llm_configs(&[], None);
        let env = crate::config::llm_configs_from_env();
        let providers: Vec<&str> = resolved.iter().map(|c| c.provider.as_str()).collect();
        let env_providers: Vec<&str> = env.iter().map(|c| c.provider.as_str()).collect();
        assert_eq!(
            providers, env_providers,
            "None (CLI/legacy path) must keep the pre-fix config-file → env behavior"
        );
    }

    // ─── apply_server_expert_overrides (RENG-69) ──────────

    /// A config with two experts at their "config file" values.
    fn config_with_experts() -> crate::models::AppConfig {
        let mut cfg: crate::models::AppConfig =
            serde_json::from_value(serde_json::json!({})).expect("empty AppConfig must deserialize");
        for (name, enabled, weight) in [("security", true, 60u8), ("quality", true, 40u8)] {
            cfg.review_experts.insert(
                name.to_string(),
                ExpertTomlDef {
                    enabled,
                    weight,
                    // A non-empty role: `validate` rejects an enabled expert
                    // without one, and `build_expert_defs` skips the invalid.
                    role: format!("{name} lead"),
                    ..Default::default()
                },
            );
        }
        cfg
    }

    /// The webhook path's rule: the persisted override wins over the file
    /// value, an untouched field keeps the file value, and an unedited expert
    /// is left completely alone.
    #[test]
    fn server_expert_overrides_win_over_the_file_values() {
        let mut overrides = crate::config::ExpertOverrides::default();
        overrides.record(
            "security",
            crate::config::ExpertOverride {
                enabled: Some(false),
                weight: Some(15),
            },
        );

        let mut config = config_with_experts();
        assert_eq!(apply_server_expert_overrides(&mut config, Some(&overrides)), 1);

        assert!(!config.review_experts["security"].enabled, "disabled by the Web UI");
        assert_eq!(
            config.review_experts["security"].weight, 15,
            "re-weighted by the Web UI"
        );
        assert!(
            config.review_experts["quality"].enabled,
            "unedited expert keeps the file value"
        );
        assert_eq!(config.review_experts["quality"].weight, 40);

        // …and the expert set a review runs follows the override.
        let names: Vec<String> = config.build_expert_defs().into_iter().map(|e| e.name).collect();
        assert_eq!(names, vec!["quality".to_string()], "only the edited expert is dropped");
    }

    /// `None` = no server state (CLI/legacy/tests): nothing is applied, so the
    /// config file's team stands exactly as before 0.10.24.
    #[test]
    fn no_server_state_leaves_the_file_values_untouched() {
        let mut config = config_with_experts();
        assert_eq!(apply_server_expert_overrides(&mut config, None), 0);
        assert!(config.review_experts["security"].enabled);
        assert_eq!(config.review_experts["security"].weight, 60);
        assert_eq!(config.build_expert_defs().len(), 2);
    }

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
            parse_error: None,
            raw_dump_path: None,
            llm_provider: None,
            llm_model: None,
            llm_fp: None,
        }];
        let agg = Some(AggregatedReport {
            findings: vec![],
            markdown: String::new(),
            raw_llm_response: String::new(),
            parse_error: None,
            raw_dump_path: None,
            llm_provider: None,
            llm_model: None,
            llm_fp: None,
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
            parse_error: None,
            raw_dump_path: None,
            llm_provider: None,
            llm_model: None,
            llm_fp: None,
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
            parse_error: None,
            raw_dump_path: None,
            llm_provider: None,
            llm_model: None,
            llm_fp: None,
        };
        let reports = vec![ExpertReport {
            expert_name: "security".to_string(),
            findings: Default::default(),
            markdown: String::new(),
            raw_llm_response: String::new(),
            parse_error: None,
            raw_dump_path: None,
            llm_provider: None,
            llm_model: None,
            llm_fp: None,
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
            parse_error: None,
            raw_dump_path: None,
            llm_provider: None,
            llm_model: None,
            llm_fp: None,
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
            parse_error: None,
            raw_dump_path: None,
            llm_provider: None,
            llm_model: None,
            llm_fp: None,
        }];
        let output = build_review_output_from_reports(reports, None);
        assert!(output.aggregated.is_none());
    }

    // ─── gate_unchanged_content (RENG-62) ────────────────────────────

    /// The explicit-rerun policy: a bypassed gate never skips, and records
    /// nothing (the review that follows owns `complete`).
    #[tokio::test]
    async fn bypassed_gate_never_skips() {
        use crate::server::dispatcher::ShouldStart;
        const DIFF: &str = "diff --git a/src/main.rs b/src/main.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
        d.complete("mr1", "sha1", Some(&content_fingerprint(DIFF))).await;
        assert_eq!(d.try_start("mr1", "note_x").await, ShouldStart::Go);

        assert_eq!(
            gate_unchanged_content(&d, ContentGate::Bypassed, "mr1", "note_x", DIFF).await,
            None,
            "an explicit rerun must proceed regardless of the fingerprint"
        );
        // Still running: the caller runs the review and completes the dispatch.
        assert_eq!(d.try_start("mr1", "sha2").await, ShouldStart::InProgress);
    }

    /// The webhook policy: identical content is reported as a skip, with the
    /// reason the caller logs.
    #[tokio::test]
    async fn enabled_gate_skips_identical_content() {
        use crate::server::dispatcher::ShouldStart;
        const DIFF: &str = "diff --git a/src/main.rs b/src/main.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
        d.complete("mr1", "sha1", Some(&content_fingerprint(DIFF))).await;
        assert_eq!(d.try_start("mr1", "sha2").await, ShouldStart::Go);

        assert_eq!(
            gate_unchanged_content(&d, ContentGate::Enabled, "mr1", "sha2", DIFF).await,
            Some("skipping: content unchanged (sha sha1 → sha2)".to_string())
        );
        // Changed content → the gate lets the review through.
        assert_eq!(d.try_start("mr1", "sha3").await, ShouldStart::Go);
        assert_eq!(
            gate_unchanged_content(&d, ContentGate::Enabled, "mr1", "sha3", "other diff").await,
            None
        );
    }
}

/// TLS (HTTPS) listener configuration for [`serve`].
///
/// Both paths must point to PEM-encoded files: `cert_path` is the leaf
/// certificate chain (leaf first, then intermediates), `key_path` the
/// unencrypted PKCS#8 private key. Providing a value makes [`serve`] bind a
/// second, HTTPS-only listener on `tls_port` in addition to the plain HTTP
/// listener; both listeners are served concurrently.
#[derive(Debug, Clone)]
pub struct TlsConfig {
    /// Path to the PEM certificate chain.
    pub cert_path: std::path::PathBuf,
    /// Path to the PEM private key.
    pub key_path: std::path::PathBuf,
    /// Port for the HTTPS listener.
    pub tls_port: u16,
}

impl TlsConfig {
    /// Build a TLS listener configuration from the two PEM paths and the
    /// HTTPS port.
    pub fn new(cert_path: std::path::PathBuf, key_path: std::path::PathBuf, tls_port: u16) -> Self {
        Self {
            cert_path,
            key_path,
            tls_port,
        }
    }
}

/// Bind a TCP listener, failing fast with an actionable message when the
/// address is already in use. `flag` names the CLI flag that controls the
/// port so the error hints at the right knob (`--port` vs `--tls-port`).
async fn bind_listener(addr: &str, port: u16, flag: &str) -> anyhow::Result<tokio::net::TcpListener> {
    use anyhow::Context as _;
    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => Ok(listener),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            anyhow::bail!(
                "Address already in use (port {port}): {addr} is taken by another process — stop it or pass {flag}"
            );
        }
        Err(e) => Err(e).with_context(|| format!("failed to bind {addr}")),
    }
}

/// Start the health check and webhook server.
///
/// The plain HTTP listener always binds on `{bind}:{port}`. When `tls` is
/// `Some`, a second HTTPS listener (axum-server over rustls) binds on
/// `{bind}:{tls_port}` and both listeners are served concurrently, so HTTP
/// and HTTPS coexist on different ports.
///
/// Failure contract: bind failures return immediately with the target
/// address in the error. `AddrInUse` additionally names the port in an
/// `Address already in use (port N)` message so the CLI can fail fast with
/// an actionable stderr line. On success a one-line startup banner per
/// listener goes to stdout (the full log stream stays in `logs.ndjson`).
pub async fn serve(
    port: u16,
    bind: &str,
    tls: Option<TlsConfig>,
    state: Arc<AppState>,
    auth: Arc<AuthConfig>,
    webhook_handlers: Vec<Arc<dyn webhook::WebhookHandler>>,
) -> anyhow::Result<()> {
    use anyhow::Context as _;

    let app = router::build(state, auth.clone(), webhook_handlers);

    let http_addr = format!("{}:{}", bind, port);
    let http_listener = bind_listener(&http_addr, port, "--port").await?;

    // Log/print only after the bind has actually succeeded: previously the
    // "listening" line was emitted before bind, so a failed start still
    // looked healthy in logs.ndjson.
    tracing::info!("Health & webhook server listening on {}", http_addr);
    let log_path = log_collector::default_ndjson_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "disabled".to_string());
    println!("review-engine listening on http://{http_addr} (health: http://{http_addr}/health, logs: {log_path})");
    if !auth.is_enabled() {
        // First-run bootstrap: the /api/v1 API is locked (401 auth_required)
        // until the initial token is set. Loopback binds can set it directly;
        // a non-loopback bind requires the one-time bootstrap key.
        if auth.bootstrap_key_required() {
            println!("  ⚠  no API token configured — first-run bootstrap on bind '{bind}': set the initial token via PUT /api/v1/system/token with header `X-Bootstrap-Key` (or use --api-token / REVIEW_API_TOKEN)");
        } else {
            println!("  ⚠  no API token configured — first-run bootstrap (loopback): set the initial token via the web UI (PUT /api/v1/system/token)");
        }
    }

    let http_app = app.clone();
    let http_future = async {
        axum::serve(http_listener, http_app)
            .await
            .with_context(|| format!("server on {http_addr} terminated unexpectedly"))
    };

    match tls {
        Some(tls_config) => {
            let tls_addr = format!("{}:{}", bind, tls_config.tls_port);
            let tls_listener = bind_listener(&tls_addr, tls_config.tls_port, "--tls-port").await?;
            // rustls 0.23 only auto-selects a CryptoProvider when exactly one
            // backend is compiled in; here both ring and aws-lc-rs are present
            // (see the Cargo.toml note), so without an explicit install the
            // TLS accept loop panics. Prefer ring — the same backend reqwest
            // already uses for LLM calls. `install_default` only fails when a
            // provider was already installed, in which case we keep the
            // caller's choice.
            let _ = rustls::crypto::ring::default_provider().install_default();
            let rustls_config =
                axum_server::tls_rustls::RustlsConfig::from_pem_file(&tls_config.cert_path, &tls_config.key_path)
                    .await
                    .with_context(|| format!("failed to load TLS certificate/key for {tls_addr}"))?;

            tracing::info!("Health & webhook server listening on https://{tls_addr}");
            println!("review-engine listening on https://{tls_addr} (health: https://{tls_addr}/health)");

            let tls_future = async {
                let std_listener = tls_listener
                    .into_std()
                    .with_context(|| format!("failed to adopt TLS listener on {tls_addr}"))?;
                // axum-server 0.8: `from_tcp_rustls` is fallible (the std →
                // tokio listener conversion can fail) and returns
                // `io::Result<Server<_>>`.
                axum_server::tls_rustls::from_tcp_rustls(std_listener, rustls_config)
                    .with_context(|| format!("failed to initialize TLS server on {tls_addr}"))?
                    .serve(app.into_make_service())
                    .await
                    .with_context(|| format!("server on {tls_addr} terminated unexpectedly"))
            };

            tokio::try_join!(http_future, tls_future)?;
        }
        None => http_future.await?,
    }
    Ok(())
}
