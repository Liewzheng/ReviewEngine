use std::sync::Arc;

use crate::server::api::types::ReviewSource;
use crate::team::orchestrator;

pub(crate) const MAX_STATIC_DIFF_BYTES: usize = 5 * 1024 * 1024; // 5 MB

/// Request header carrying the GitLab upstream credential for `gitlab_mr`
/// reviews (docs/rest-api.md §1 凭证传输). Distinct from the API auth header
/// (`Authorization: Bearer` / `X-API-Key`) and from the same-named header on
/// the inbound `/webhook/gitlab` route, where it carries the webhook secret.
pub(crate) const GITLAB_TOKEN_HEADER: &str = "x-gitlab-token";

/// Resolve the GitLab upstream credential for a review request.
///
/// Precedence (docs/rest-api.md §1): the `X-Gitlab-Token` request header
/// wins; when absent/blank, a configured git platform whose `base_url` — or,
/// when set, `internal_base_url` — scheme-less `host[:port]` matches the MR
/// URL supplies the token for that instance; when no platform matches (or the
/// match has no token), the legacy server-side token is used — the GitLab
/// runtime config seeded at startup from `--gitlab-token` / `GITLAB_TOKEN` and
/// mutable via `PUT /api/v1/config`. Returns `None` when no source yields a
/// token; callers turn that into a `400`.
pub(crate) fn resolve_gitlab_token(
    header: Option<&str>,
    mr_url: Option<&str>,
    platforms: &[crate::models::GitPlatformConfig],
) -> Option<String> {
    if let Some(t) = header.map(str::trim).filter(|t| !t.is_empty()) {
        return Some(t.to_string());
    }
    // A matched platform with an empty token "yields" nothing — the chain
    // continues to the legacy default (first non-empty token wins), exactly
    // like a blank header falls through to the server-side lookup.
    if let Some(url) = mr_url {
        // Review-URL identity (RENG-33): `base_url` or the entry's own
        // `internal_base_url`, so a submission of either configured address
        // resolves to that platform's token. Both are addresses the entry
        // explicitly configured, so the token still cannot flow to a port the
        // user never wrote down (unlike inbound webhook verification, which
        // folds a uniquely-matched host — see find_git_platform_for_url).
        if let Some(platform) = crate::models::find_git_platform_for_review_url(platforms, url) {
            if !platform.token.trim().is_empty() {
                return Some(platform.token.clone());
            }
        }
    }
    crate::server::gitlab::gitlab_runtime()
        .read()
        .ok()
        .map(|rt| rt.token.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// A resolved review source: the raw diff plus, for MR-based sources, the MR
/// metadata fetched from the provider API. `mr_info` is `Some` only for
/// `gitlab_mr` sources today; local/static sources carry no MR context.
pub(crate) struct ResolvedSource {
    pub diff: String,
    pub mr_info: Option<crate::models::MRInfo>,
    /// Rendered AGENTS.md prompt section (RENG-18). `None` when disabled,
    /// missing, or unavailable.
    pub agents_md: Option<String>,
    /// Provider-API ground truth for the adjudication pass (RENG-31), built
    /// while the MR client and its credential are in hand. `None` for local
    /// (the pass reads the checkout) and static-diff sources.
    pub file_source: Option<std::sync::Arc<dyn crate::team::file_source::FileSource>>,
}

impl std::fmt::Debug for ResolvedSource {
    /// The `FileSource` is summarised by kind: the diff body would drown the
    /// output and the source has nothing else to say about itself.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedSource")
            .field("diff_bytes", &self.diff.len())
            .field("mr_info", &self.mr_info)
            .field("agents_md", &self.agents_md.is_some())
            .field("file_source", &self.file_source.as_ref().map(|s| s.kind()))
            .finish()
    }
}

/// The outcome of one review run, as the task runner persists it.
#[derive(Debug)]
pub(crate) struct ReviewOutcome {
    /// The serialized [`crate::models::ReviewOutput`] (`reviews.result`).
    pub value: serde_json::Value,
    /// Human-readable one-line summary for the log and the completion event.
    pub summary: String,
    /// RENG-77: the `reviews.llm_summary` JSON, computed from the IN-MEMORY
    /// output because that is the only place the serving cards' fingerprints
    /// exist. `None` when the run recorded no LLM attribution.
    pub llm_summary: Option<String>,
}

/// Run the review for a resolved source.
///
/// `expert_overrides` carries the persisted WebUI expert edits (RENG-69):
/// this path resolves its config from the request's inline TOML (or the config
/// file), never from `AppState::app_config`, so the override map is re-applied
/// here — otherwise disabling an expert in the WebUI would keep affecting
/// nothing but the management page.
///
/// `llm_sink` (RENG-57) receives one latency sample per LLM call attempt and is
/// forwarded to the orchestrator unchanged.
pub(crate) async fn run_review(
    resolved: ResolvedSource,
    config_toml: Option<String>,
    llm_configs: Vec<crate::models::LLMConfig>,
    llm_sink: Option<std::sync::Arc<dyn crate::llm::sampling::LlmCallSink>>,
    expert_overrides: Arc<crate::config::ExpertOverrides>,
) -> anyhow::Result<ReviewOutcome> {
    let config_source = config_toml.map(crate::models::ConfigSource::Inline);
    let mut app_config = crate::config::resolve_config(config_source).await?;
    // DB overrides win over the config file's `[review_experts]` (the file is
    // the base/default), exactly as on startup.
    let applied = expert_overrides.apply_to(&mut app_config);
    if applied > 0 {
        tracing::debug!(applied, "applied persisted expert overrides to the REST review config");
    }

    let experts = app_config.build_expert_defs();
    // MR-based reviews reuse the freshly fetched metadata so prompts carry the
    // real title/branches; local/static sources keep the placeholder context.
    let mut mr_info = resolved.mr_info.unwrap_or_else(|| {
        crate::models::MRInfo::new(
            "api".to_string(),
            "API Review".to_string(),
            "unknown".to_string(),
            "unknown".to_string(),
        )
    });
    // RENG-18: attach AGENTS.md context resolved alongside the diff/MR info.
    mr_info.agents_md = resolved.agents_md;

    let review_result = tokio::time::timeout(
        std::time::Duration::from_secs(600),
        orchestrator::run_experts(
            &experts,
            &mr_info,
            &resolved.diff,
            &llm_configs,
            &app_config,
            None,
            "",
            None,
            resolved.file_source,
            llm_sink,
        ),
    )
    .await;

    let (reports, _, dropped_findings, consolidated, failures) = match review_result {
        Ok(result) => result?,
        Err(_) => anyhow::bail!("Task timed out after 600 seconds"),
    };

    // RENG-77 §4: an expert that produced no report at all (an empty
    // completion, an exhausted provider chain) is only visible through the
    // error list — `reports` holds the experts that answered. Carrying it on
    // the output is what keeps a partially failed run from reading as a clean
    // one, and it is what the review detail surfaces.
    let failed = failures.len();
    let output = crate::models::ReviewOutput::new(reports)
        .with_dropped_findings(dropped_findings)
        .with_consolidated(consolidated)
        .with_errors(failures);
    let findings: usize = output.reports.iter().map(|r| r.findings.len()).sum();
    let summary = if failed == 0 {
        format!("{} expert report(s), {} finding(s)", output.reports.len(), findings)
    } else {
        format!(
            "{} expert report(s), {} finding(s), {} expert(s) failed",
            output.reports.len(),
            findings,
            failed
        )
    };
    // RENG-77: the usage snapshot must be built from THIS value, before it is
    // serialized — `LlmUsage::fp` is `skip_serializing`, so a summary derived
    // from `value` (or from the stored column) would lose the fingerprint and
    // the page could not tell two same-named cards apart.
    let llm_summary = crate::store::rows::llm_summary_from_output(&output);
    let value = serde_json::to_value(&output).unwrap_or_default();
    Ok(ReviewOutcome {
        value,
        summary,
        llm_summary,
    })
}
pub(crate) async fn resolve_source(
    source: ReviewSource,
    gitlab_token: Option<String>,
    _config: &Option<Arc<crate::models::AppConfig>>,
    inject_agents_md: bool,
) -> anyhow::Result<ResolvedSource> {
    match source {
        ReviewSource::GitLabMr { url } => {
            // Defense in depth: submit/rerun handlers already enforce the
            // credential rule with a 400; a missing token here means the
            // handler contract was bypassed.
            let token = gitlab_token
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "GitLab token required for gitlab_mr reviews: pass the X-Gitlab-Token header or configure a server-side GitLab token"
                    )
                })?;
            let client = crate::git_provider::gitlab::client::Client::new(&token, &url)?;
            // Fetch metadata before the diff: when the diff fetch (or the
            // review itself) later fails, the task runner has already
            // back-filled the record's display metadata from `mr_info`.
            let mr_info = client.fetch_mr_info().await?;
            let diff = client.fetch_diff().await?;
            // RENG-18: best-effort AGENTS.md from the MR's target branch.
            // Read+render only; persistence happens in task.rs with the real
            // task_id (review_contexts rows are keyed by task_id).
            let agents_md = if inject_agents_md {
                super::agents_md::fetch_remote_agents_md(&client, &mr_info.target_branch)
                    .await
                    .and_then(|c| super::agents_md::render_agents_md(&c))
            } else {
                None
            };
            // RENG-31: the adjudication pass needs the cited files' full
            // content, and this review never clones — fetch them through the
            // provider API at the reviewed SHA. The client and its credential
            // are already in hand here.
            let file_source = crate::team::file_source::provider_source_from_client_or_warn(&client, &mr_info.git_hash);
            Ok(ResolvedSource {
                diff,
                mr_info: Some(mr_info),
                agents_md,
                file_source,
            })
        }
        ReviewSource::LocalRepo { path, base, head } => {
            let repo_path = std::path::Path::new(&path);
            if !repo_path.exists() {
                anyhow::bail!("Repository path does not exist: {}", path);
            }
            if !repo_path.is_dir() {
                anyhow::bail!("Repository path is not a directory: {}", path);
            }
            if let Some(ref base_ref) = base {
                crate::git::local::validate_ref(base_ref)?;
            }
            if let Some(ref head_ref) = head {
                crate::git::local::validate_ref(head_ref)?;
            }
            let browser = crate::git::local::LocalGitBrowser::new(&path);
            let diff = browser
                .get_diff(base.as_deref().unwrap_or("main"), head.as_deref(), false, None, None)
                .await?;
            // RENG-18: best-effort AGENTS.md from the local checkout.
            // Read+render only; persistence happens in task.rs with the real
            // task_id (review_contexts rows are keyed by task_id).
            let agents_md = if inject_agents_md {
                super::agents_md::read_local_agents_md(repo_path, super::agents_md::DEFAULT_MAX_FILE_BYTES)
                    .and_then(|c| super::agents_md::render_agents_md(&c))
            } else {
                None
            };
            Ok(ResolvedSource {
                diff,
                mr_info: None,
                agents_md,
                // RENG-31: the placeholder MR context of a local-path review
                // names the review `api`, not a filesystem path, so hand the
                // adjudicator the checkout explicitly instead of letting it
                // fall back to "no ground truth".
                file_source: Some(Arc::new(crate::team::file_source::LocalFileSource::new(&path))),
            })
        }
        ReviewSource::StaticDiff { diff } => {
            if diff.len() > MAX_STATIC_DIFF_BYTES {
                anyhow::bail!(
                    "Static diff exceeds maximum size of {} MB",
                    MAX_STATIC_DIFF_BYTES / (1024 * 1024)
                );
            }
            Ok(ResolvedSource {
                diff,
                mr_info: None,
                agents_md: None,
                file_source: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ExpertOverride, ExpertOverrides};
    use crate::models::ConfigSource;

    /// Hermetic inline config: `resolve_config(Inline(_))` = the shipped
    /// defaults + this TOML, with no config-file / `$HOME` read.
    const INLINE: &str = "[commands]\nreview = true\n";

    /// (RENG-69) The REST review path resolves its own config (request TOML,
    /// else the config file) instead of reading `AppState::app_config`, so it
    /// must re-apply the persisted expert overrides. Disabling EVERY shipped
    /// expert through the override map leaves the review with an empty team —
    /// observable as a review that runs no expert at all (and therefore needs
    /// no LLM provider / network).
    #[tokio::test]
    async fn persisted_expert_overrides_apply_to_the_rest_review_config() {
        // Control: without overrides the same inline config has a real team.
        let plain = crate::config::resolve_config(Some(ConfigSource::Inline(INLINE.to_string())))
            .await
            .expect("inline config must resolve");
        let plain_names: Vec<String> = plain.build_expert_defs().into_iter().map(|e| e.name).collect();
        assert!(
            !plain_names.is_empty(),
            "the shipped defaults must provide experts, else this test proves nothing"
        );

        // The WebUI disabled every one of them.
        let mut overrides = ExpertOverrides::default();
        for name in plain.review_experts.keys() {
            overrides.record(
                name,
                ExpertOverride {
                    enabled: Some(false),
                    weight: None,
                },
            );
        }

        let resolved = ResolvedSource {
            diff: "diff --git a/src/a.rs b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n".to_string(),
            mr_info: None,
            agents_md: None,
            file_source: None,
        };
        let outcome = run_review(
            resolved,
            Some(INLINE.to_string()),
            vec![],
            // RENG-57: no store in this test → no latency samples to record.
            None,
            Arc::new(overrides),
        )
        .await
        .expect("an empty expert team is a legitimate (empty) review");

        let value = outcome.value;
        assert_eq!(
            value["reports"].as_array().map(Vec::len),
            Some(0),
            "every expert was disabled by the override: {value}"
        );
        assert_eq!(outcome.summary, "0 expert report(s), 0 finding(s)");
        assert_eq!(outcome.llm_summary, None, "no expert ran, so no LLM usage was recorded");
    }

    /// RENG-77 §4 end-to-end: a single-entry provider chain that returns
    /// an empty completion is surfaced as a failed review with the
    /// diagnosis in the error message — exactly what the LLM page surfaces
    /// through `GET /llm/providers`' failure count and what the user sees
    /// in the queue status. The runner's `ReviewOutcome.summary` names
    /// the failure so the log line and the completion event both tell the
    /// same story (no "0 expert report(s), 0 finding(s)" masquerading as a
    /// clean, high-scoring pass).
    #[tokio::test]
    async fn an_empty_completion_makes_the_review_fail_with_a_diagnosis() {
        let server = wiremock::MockServer::start().await;
        // The provider answers with an empty `content` field — the measured
        // RENG-77 §4 case (a reasoning model spending its whole
        // `max_tokens` budget on `reasoning_tokens`).
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"content": ""}}],
                "usage": {"total_tokens": 4096},
                "model": "deepseek-v4-flash",
            })))
            .mount(&server)
            .await;

        let resolved = ResolvedSource {
            diff: "diff --git a/src/a.rs b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n".to_string(),
            mr_info: None,
            agents_md: None,
            file_source: None,
        };
        let mut config = crate::models::LLMConfig {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-flash".to_string(),
            api_key: "sk-test".to_string(),
            api_base: server.uri(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
            disabled: false,
        };
        // No provider registry → the direct OpenAI-compatible path runs.
        config.api_key = "sk-test".to_string();

        let outcome = crate::server::api::review::resolve::run_review(
            resolved,
            Some(INLINE.to_string()),
            vec![config],
            // RENG-57: no store in this test → no latency samples to record.
            None,
            Arc::new(crate::config::ExpertOverrides::default()),
        )
        .await;
        assert!(
            outcome.is_err(),
            "a single-entry chain returning an empty completion must fail the review, \
             not present as a clean pass: {:?}",
            outcome
        );
        let err = format!("{:#}", outcome.unwrap_err());
        assert!(
            err.contains("empty completion"),
            "the failure's cause chain names the diagnosis the user needs (§4): {err}"
        );
        assert!(
            err.contains("deepseek") || err.contains("deepseek-v4-flash"),
            "the failure names the provider / model the user already sees on the card: {err}"
        );
    }

    /// RENG-77 §2, on the REAL handler and store path: `run_review` — the
    /// function `enqueue_review` calls — builds `reviews.llm_summary` from the
    /// IN-MEMORY output (the only place a serving card's `fp` exists) and hands
    /// it to the write-through, which is what lets `GET /llm/providers`
    /// attribute the review to the card the user configured.
    ///
    /// Nothing is hand-built: the provider is a real HTTP mock, the fingerprint
    /// is the one `LLMConfig::entry_fp()` derives from the card's own
    /// credentials, the summary is persisted into a real SQLite `reviews` row,
    /// and `requestCount` / `usageShare` are read back through
    /// `UsageSnapshot` — the exact aggregate the endpoint serves. The test also
    /// pins the security contract: the fp reaches `reviews.llm_summary` and
    /// never `reviews.result`.
    #[tokio::test]
    async fn the_review_runner_persists_an_llm_summary_carrying_the_configured_cards_fp() {
        use crate::server::api::llm_usage::{window_start, UsageSnapshot};
        use crate::server::task_queue::{record_task_started, SourceMeta, TaskState, TaskStore};
        use crate::store::traits::ReviewStore;

        let server = wiremock::MockServer::start().await;
        // A provider that answers, but echoes an ALIAS of the configured model
        // (§3): every consumer looks the model up by what the card says, so the
        // summary must carry `deepseek-v4-flash`, never `deepseek-flash`.
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"content": "## Review\n\nNothing to report.\n"}}],
                "usage": {"total_tokens": 21},
                "model": "deepseek-flash",
            })))
            .mount(&server)
            .await;

        let card = crate::models::LLMConfig {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-flash".to_string(),
            api_key: "sk-reng-77-end-to-end".to_string(),
            api_base: server.uri(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: Some(true),
            disabled: false,
        };
        let expected_fp = card.entry_fp();

        let resolved = ResolvedSource {
            diff: "diff --git a/src/a.rs b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n".to_string(),
            mr_info: None,
            agents_md: None,
            file_source: None,
        };
        let outcome = run_review(
            resolved,
            Some(INLINE.to_string()),
            vec![card],
            // RENG-57: no sample sink here — the usage summary is the subject.
            None,
            Arc::new(ExpertOverrides::default()),
        )
        .await
        .expect("the mock provider answers every expert");

        let summary = outcome
            .llm_summary
            .clone()
            .expect("a review that ran experts records their usage");
        let parsed: serde_json::Value = serde_json::from_str(&summary).expect("the summary is JSON");
        let entry = parsed.as_array().expect("array").first().expect("at least one usage");
        assert_eq!(
            entry["fp"], expected_fp,
            "the summary carries the CONFIGURED card's fingerprint: {summary}"
        );
        assert_eq!(
            entry["model"], "deepseek-v4-flash",
            "the configured model, not the provider's alias: {summary}"
        );
        assert!(
            !summary.contains("\"deepseek-flash\""),
            "the alias must not leak into the summary: {summary}"
        );

        // The security contract: the fingerprint is in `llm_summary` only.
        let serialized_result = outcome.value.to_string();
        assert!(
            !serialized_result.contains(&expected_fp),
            "the fp must never reach the serialized result / API response"
        );

        // The write-through the REST handler performs.
        let db = Arc::new(crate::store::SqlxStore::new_in_memory().await.unwrap());
        db.migrate().await.unwrap();
        let mut store = TaskStore::new();
        store.set_db(db.clone());
        let task_id = record_task_started(&store, SourceMeta::default()).await;
        store
            .update_with_summary(
                task_id,
                TaskState::Completed,
                Some(outcome.value),
                None,
                outcome.llm_summary,
            )
            .await;

        // Read it back the way `GET /llm/providers` does.
        let now = chrono::Utc::now();
        let since = window_start(now);
        let stats = db.llm_usage_since(since).await.expect("in-memory store is attached");
        let usage = UsageSnapshot::new(since, stats).for_card("deepseek", "deepseek-v4-flash", &expected_fp, false);
        assert_eq!(
            usage.request_count,
            Some(1),
            "the page's requestCount attributes the review to the configured card"
        );
        assert_eq!(usage.usage_share, Some(1.0), "the only usage in the window is this one");
        assert_eq!(
            usage.success_rate,
            Some(1.0),
            "the review completed, so the card shows a terminal outcome"
        );

        let stored_summary: String = sqlx::query_scalar("SELECT llm_summary FROM reviews WHERE task_id = ?")
            .bind(task_id.to_string())
            .fetch_one(db.pool())
            .await
            .expect("the write-through persists the summary");
        assert!(
            stored_summary.contains(&expected_fp),
            "the persisted column — not just the in-memory value — carries the fp: {stored_summary}"
        );
    }
}
