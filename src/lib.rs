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
pub mod catalog;
pub mod config;
pub mod context;
pub mod coverage;
pub mod diff;
/// Storage diagnosis and self-repair: `reng doctor [--fix]` (RENG-106).
pub mod doctor;
pub mod error;
pub mod expert;
pub mod feedback;
pub mod git;
pub mod git_provider;
pub mod input;
pub mod language;
pub mod llm;
/// Prometheus metrics used by the server, CLI, and LLM client.
pub mod metrics;
pub mod models;
pub mod output;
/// The process-wide data directory: the root every persisted state path
/// resolves under (`serve --data-dir`, RENG-37).
pub mod paths;
pub mod progress;
pub mod prompt;
pub mod publisher;
pub mod repo;
pub mod scoring;
pub mod server;
/// Persistence layer (0.10.0): sqlx `Any` pool serving PostgreSQL and SQLite
/// from one code path. See `design/persistence.md`.
pub mod store;
pub mod team;
pub mod tokenizer;

/// Self-update support: GitHub Releases check, platform asset mapping,
/// download + SHA-256 verification, safe extraction, install-method hints.
/// Shared by the CLI upgrade command and the web status endpoint.
pub mod upgrade;

/// Optional PyO3 bindings for calling review-engine from Python.
/// Only compiled when the `python` feature is enabled.
#[cfg(feature = "python")]
pub mod python;

/// Helpers shared by the crate's own unit tests. Test-only: absent from the
/// shipped library and invisible to the `tests/` integration crate, which sees
/// the library built without `cfg(test)`.
#[cfg(test)]
mod test_util;

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
    token: &str,
    llm_configs: Vec<LLMConfig>,
    config_source: Option<ConfigSource>,
    progress_override: Option<(crate::progress::ProgressMap, String)>,
    dump_dir: Option<std::path::PathBuf>,
) -> Result<ReviewOutput> {
    let config = config::resolve_config(config_source.clone()).await?;

    let is_github = mr_url.contains(".github.") || mr_url.contains("github.com");
    let (mr_info, diff, app_config) = if is_github {
        let github_client = git_provider::github::client::Client::new(token, mr_url)?;
        let mr_info = github_client.fetch_pr_info().await?;
        let diff = github_client.fetch_diff().await?;
        // GitHub client does not support fetching config TOML from repo
        let app_config = config.clone();
        (mr_info, diff, app_config)
    } else {
        let gitlab_client = git_provider::gitlab::client::Client::new(token, mr_url)?;
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
        (mr_info, diff, app_config)
    };

    let experts = app_config.build_expert_defs();

    let aggregated = app_config.report.aggregated && experts.iter().any(|e| e.name == "aggregator");

    let (progress_map, review_id) = match progress_override {
        Some((map, id)) => (map, id),
        None => (crate::progress::new_progress_map(), uuid::Uuid::new_v4().to_string()),
    };

    // The review never clones the repository: the adjudication pass (when
    // enabled) gets full-file ground truth through the provider API at the
    // reviewed SHA, so the false-positive filter works here too (RENG-31).
    let remote_files = crate::team::file_source::provider_source_or_warn(mr_url, token, &mr_info.git_hash);

    let (findings, global_context, dropped_findings, consolidated, expert_failures) =
        crate::team::orchestrator::run_experts(
            &experts,
            &mr_info,
            &diff,
            &llm_configs,
            &app_config,
            Some(progress_map.clone()),
            &review_id,
            dump_dir,
            remote_files,
            // RENG-57: the CLI has no sample store to write to (only `serve`
            // owns one), so no LLM call latency is recorded here.
            None,
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
                    None,
                )
                .await?;
                ReviewOutput::with_aggregated(findings, aggregated_report)
            }
            None => ReviewOutput::new(findings),
        }
    } else {
        ReviewOutput::new(findings)
    };
    let output = output
        .with_dropped_findings(dropped_findings)
        .with_consolidated(consolidated)
        // RENG-77 §4: experts that produced no report, with the reason each saw.
        .with_errors(expert_failures);

    // Mark progress complete
    crate::progress::complete_progress(Some(&progress_map), &review_id);

    Ok(output)
}

/// Format a [`PublishRefusal`](crate::models::PublishRefusal) for the
/// publish-time error message.
///
/// Each hit's `kind`, `key` (when present), length and fingerprint prefix
/// are named — exactly the context the operator needs to find the offending
/// line locally without the value itself being echoed. The full body is
/// dropped from the message: surfacing the rendered text in an error path
/// would itself be a leak vector (any logger / formatter that calls
/// `Debug` on this type would print the body — see [`PublishRefusal`]).
pub(crate) fn format_publish_refusal(refusal: &crate::models::PublishRefusal, surface: &str) -> String {
    let mut msg = format!(
        "refusing to publish: {review} body carries {n} suspected plaintext credential(s); \
         re-redact the offending finding locally before retrying. Detected:",
        review = surface,
        n = refusal.hits.len(),
    );
    for hit in &refusal.hits {
        let key = hit.key.as_deref().unwrap_or("(no key)");
        // 8 hex chars is enough to identify the value in the local file.
        // The marker in the rendered text prints the full 16-hex fp — an
        // operator who greps for the 8-hex prefix lands on the marker.
        let fp = hit.fingerprint_short();
        msg.push_str(&format!(
            "\n  - kind={kind}, key={key}, len={len}, fp={fp}",
            kind = hit.kind.label(),
            key = key,
            len = hit.value.chars().count(),
            fp = fp,
        ));
    }
    msg
}
///
/// Automatically selects the right Git provider based on the MR URL:
/// - `github.com` → `GitHubProvider`
/// - everything else → `GitLabProvider`
///
/// On failure, only logs a warning — does not return an error,
/// since the review itself has already completed successfully.
///
/// The reviewed diff is not in hand here; [`publish_review_with_diff`] looks it
/// up best-effort so the inline anchor gate still applies.
pub async fn publish_review(token: &str, mr_url: &str, output: &ReviewOutput) -> Result<()> {
    publish_review_with_diff(token, mr_url, output, None).await
}

/// Publish review results, gating inline notes by the diff the review ran on.
///
/// `diff` is the unified diff the review reviewed (the server path has it in
/// hand); when it is `None` the diff is fetched from the provider so the anchor
/// gate still applies. If the diff cannot be determined the gate is disabled —
/// fail-open, because a missing index must never silently drop every note.
///
/// Which findings actually become inline notes is decided by
/// [`crate::publisher::PublishPolicy`] (thresholds, a per-round cap, and
/// summary-only publishing for a documentation/CI-only change); everything the
/// policy withholds stays in the board, and a plan that rolled findings up is
/// reported in the board's inline-notes section (RENG-63).
pub async fn publish_review_with_diff(
    token: &str,
    mr_url: &str,
    output: &ReviewOutput,
    diff: Option<&str>,
) -> Result<()> {
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

    let mut errors: Vec<anyhow::Error> = Vec::new();

    // Inline notes are drawn from the consolidated finding set (dedup +
    // adjudication applied) and pass the delivery policy before anything is
    // posted: thresholds, a per-round cap, and summary-only publishing when
    // every changed file is documentation or CI glue (RENG-63). The plan is
    // computed first so the board can state what was withheld from inline
    // delivery; resolve the diff only when the policy admits something.
    let policy = crate::publisher::PublishPolicy::from_env();
    tracing::info!("Inline-note delivery policy: {}", policy.describe());

    let has_candidates = crate::publisher::has_inline_candidates(output, &policy);
    let diff_index = if has_candidates {
        match diff {
            Some(text) => Some(crate::publisher::DiffIndex::from_diff(text)),
            None => match provider.fetch_diff().await {
                Ok(text) => Some(crate::publisher::DiffIndex::from_diff(&text)),
                Err(e) => {
                    tracing::warn!("Could not fetch the diff for the inline-note anchor check: {e}");
                    None
                }
            },
        }
        // An index that parsed nothing (empty or oversized diff) is treated as
        // "unavailable" rather than as "nothing is in the diff".
        .filter(|index| !index.is_empty())
    } else {
        None
    };
    if has_candidates && diff_index.is_none() {
        tracing::warn!("Inline notes will be posted without a diff anchor check (diff unavailable)");
    }
    // An unavailable diff means an unknown change set, which is never
    // downgraded to summary-only (the detection fails open).
    let docs_only = diff_index
        .as_ref()
        .is_some_and(|index| crate::publisher::is_docs_or_ci_only(index.changed_files()));
    if docs_only {
        tracing::info!(
            "Documentation/CI-only change: publishing a summary only, no inline notes{}",
            if policy.inline_on_docs_only {
                " (overridden by REVIEW_PUBLISH_INLINE_ON_DOCS_ONLY)"
            } else {
                ""
            }
        );
    }
    let plan = crate::publisher::plan_inline_notes_for_output(output, diff_index.as_ref(), docs_only, &policy);

    let md = crate::publisher::render_board(output, &plan, &policy);
    // RENG-113 publish-time gate: every body we are about to send to the
    // provider (the board, every inline note) is scanned for plaintext
    // credentials. The renderer redacts Evidence blocks and other LLM-
    // derived prose at the source (the report file is already safe), so
    // the gate is a defence-in-depth: a value that slipped through a
    // renderer path we forgot to update still blocks the post instead of
    // landing in the MR discussion history. A hit refuses the whole
    // publish, board and inline notes alike — the operator can then
    // re-mask the offending finding locally and retry.
    if let Err(refusal) = crate::models::scan_for_publish(&md) {
        return Err(anyhow::anyhow!(format_publish_refusal(&refusal, "board")));
    }
    for finding in &plan.selected {
        let body = crate::publisher::format_inline_body(finding);
        if let Err(refusal) = crate::models::scan_for_publish(&body) {
            return Err(anyhow::anyhow!(format_publish_refusal(
                &refusal,
                &format!("inline note for `{}:{}`", finding.file, finding.line.unwrap_or(0)),
            )));
        }
    }
    // The board's id, when it went up: the inline-note failures can only be
    // reported in the board if there is a board to update.
    let board_id = match provider.find_or_update_discussion(&md).await {
        Ok(id) => Some(id),
        Err(e) => {
            errors.push(e.context("discussion"));
            None
        }
    };

    let summary = crate::publisher::publish_planned_inline_notes(&*provider, &plan).await;
    if summary.failed > 0 {
        // RENG-99: the board is the report the user reads, and it had to be
        // posted before the notes were (the plan is what decides which notes
        // exist). So the notes that were refused are written into it afterwards
        // — "N 条行内评论未能发布" with the provider's verdict for each, instead
        // of the single WARN this used to be.
        if let Some(board_id) = board_id {
            // The appended section starts with its own `##` heading; the blank
            // line keeps it out of a bullet list the board may end with (the
            // dropped-findings appendix can).
            let md = format!("{md}\n{}", crate::publisher::render_inline_failure_section(&summary));
            if let Err(e) = provider.update_discussion(&board_id, &md).await {
                // Not a second error: the count this addendum carries is already
                // reported below, and the addendum is another rendering of it.
                tracing::warn!("Could not append the inline-note failures to the review board: {e}");
            }
        }
        // The batch is already finished; surface the partial failure instead of
        // hiding it in the log (the pre-0.10.13 code did the opposite: the
        // first failure ended the batch and the rest were lost silently).
        errors.push(anyhow::anyhow!(
            "{} inline note(s) failed to publish ({} posted, {} rolled up, {} skipped)",
            summary.failed,
            summary.posted,
            summary.rolled_up,
            summary.skipped
        ));
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

#[cfg(test)]
mod publish_policy_e2e_tests {
    //! End-to-end coverage of the RENG-63 delivery policy through the real
    //! publish path: `publish_review_with_diff` against a mock GitLab, so the
    //! board assembly, the policy plan and the inline POSTs are all production
    //! code — only the provider's HTTP endpoint is faked.

    use crate::models::{Effort, ExpertReport, Finding, ReviewOutput, Severity};
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn finding(file: &str, line: u32, title: &str) -> Finding {
        Finding {
            file: file.to_string(),
            line: Some(line),
            line_end: None,
            severity: Severity::High,
            confidence: 9,
            category: "security".to_string(),
            title: title.to_string(),
            summary: String::new(),
            evidence: String::new(),
            impact: String::new(),
            recommendation: "Propagate the error instead of ignoring it".to_string(),
            effort: Effort::Small,
            expert_name: "security".to_string(),
            expert_role: String::new(),
            agrees_with: Vec::new(),
            references: Vec::new(),
        }
    }

    fn output_with(findings: Vec<Finding>) -> ReviewOutput {
        ReviewOutput::new(vec![ExpertReport {
            expert_name: "security".to_string(),
            markdown: findings
                .iter()
                .map(|f| format!("### {}\n", f.title))
                .collect::<String>(),
            findings,
            raw_llm_response: String::new(),
            parse_error: None,
            raw_dump_path: None,
            llm_provider: None,
            llm_model: None,
            llm_fp: None,
        }])
    }

    /// Build a `ReviewOutput` whose `report.markdown` is the actual
    /// production `render_expert_markdown` output — the same code path
    /// `render_expert_section` reads and `render_board` composes into the
    /// board body. The P1 regression (round 1 missed this; round 2 pins
    /// it) needs the production renderer in the loop because the marker
    /// only fires on a redacted body, and the renderer is what produces
    /// the redacted body.
    fn output_via_render(findings: Vec<Finding>) -> ReviewOutput {
        let markdown = crate::output::renderer::render_expert_markdown("security", &findings);
        ReviewOutput::new(vec![ExpertReport {
            expert_name: "security".to_string(),
            markdown,
            findings,
            raw_llm_response: String::new(),
            parse_error: None,
            raw_dump_path: None,
            llm_provider: None,
            llm_model: None,
            llm_fp: None,
        }])
    }

    /// A one-hunk unified diff that changes line 1 of each named file — the
    /// line every finding in these fixtures anchors to.
    fn diff_for(files: &[&str]) -> String {
        files
            .iter()
            .map(|file| {
                format!(
                    "diff --git a/{file} b/{file}\n\
                     index 1111111..2222222 100644\n\
                     --- a/{file}\n\
                     +++ b/{file}\n\
                     @@ -1,1 +1,2 @@\n\
                     +changed\n"
                )
            })
            .collect()
    }

    /// Mount the GitLab endpoints the publish path touches: the current user,
    /// the discussion list (empty → the board is created), MR info (for the
    /// inline anchor), note creation (the board) and discussion creation (an
    /// inline note).
    async fn mount_gitlab(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/api/v4/user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 1})))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/group%2Fproject/merge_requests/1/discussions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/group%2Fproject/merge_requests/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "title": "t",
                "source_branch": "a",
                "target_branch": "b",
                "author": {"id": 1, "name": "Alice"},
                "diff_refs": {"base_sha": "b1", "start_sha": "s1", "head_sha": "h1"}
            })))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v4/projects/group%2Fproject/merge_requests/1/notes"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 7})))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v4/projects/group%2Fproject/merge_requests/1/discussions"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 8})))
            .mount(server)
            .await;
    }

    /// Requests the provider received, as `(method, path, body)` triples.
    async fn received(server: &MockServer) -> Vec<(String, String, String)> {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|request| {
                (
                    request.method.as_str().to_string(),
                    request.url.path().to_string(),
                    String::from_utf8_lossy(&request.body).to_string(),
                )
            })
            .collect()
    }

    async fn board_body(server: &MockServer) -> String {
        received(server)
            .await
            .into_iter()
            .find(|(method, path, _)| method == "POST" && path.ends_with("/notes"))
            .expect("the board must be posted")
            .2
    }

    fn inline_posts(server_requests: &[(String, String, String)]) -> Vec<String> {
        server_requests
            .iter()
            .filter(|(method, path, _)| method == "POST" && path.ends_with("/discussions"))
            .map(|(_, _, body)| body.clone())
            .collect()
    }

    fn mr_url(server: &MockServer) -> String {
        format!("{}/group/project/-/merge_requests/1", server.uri())
    }

    /// Six policy-passing findings, one per file, all inside the diff → two
    /// inline notes and four rolled up into the board.
    #[tokio::test]
    async fn publish_review_caps_inline_notes_and_rolls_the_rest_into_the_board() {
        let server = MockServer::start().await;
        mount_gitlab(&server).await;

        let files = [
            "src/f1.rs",
            "src/f2.rs",
            "src/f3.rs",
            "src/f4.rs",
            "src/f5.rs",
            "src/f6.rs",
        ];
        let output = output_with(
            files
                .iter()
                .enumerate()
                .map(|(index, file)| finding(file, 1, &format!("Issue {index}")))
                .collect(),
        );
        let diff = diff_for(&files);

        crate::publish_review_with_diff("token", &mr_url(&server), &output, Some(&diff))
            .await
            .expect("publishing must succeed");

        let requests = received(&server).await;
        assert_eq!(
            inline_posts(&requests).len(),
            2,
            "the default cap is two notes per round"
        );
        assert!(inline_posts(&requests)[0].contains("src/f1.rs"));
        assert!(inline_posts(&requests)[1].contains("src/f2.rs"));

        let board = board_body(&server).await;
        assert!(board.contains("# CodeReview Board"));
        assert!(
            board.contains("exceeded the 2-note per-round cap"),
            "the board states why the remaining findings were not posted inline"
        );
        for file in ["src/f3.rs", "src/f4.rs", "src/f5.rs", "src/f6.rs"] {
            assert!(board.contains(&format!("`{file}:1`")), "{file} must be rolled up");
        }
        assert!(
            !board.contains("`src/f1.rs:1` — "),
            "the posted findings are not re-listed in the policy section"
        );
    }

    /// A documentation-only change set publishes the board and nothing else,
    /// even though the findings are Critical/High and inside the diff.
    #[tokio::test]
    async fn publish_review_publishes_a_docs_only_round_summary_only() {
        let server = MockServer::start().await;
        mount_gitlab(&server).await;

        let files = ["AGENTS.md", "docs/design.md"];
        let mut findings: Vec<Finding> = files
            .iter()
            .map(|file| finding(file, 1, "The document does not explain the workflow"))
            .collect();
        findings[0].severity = Severity::Critical;
        let output = output_with(findings);
        let diff = diff_for(&files);

        crate::publish_review_with_diff("token", &mr_url(&server), &output, Some(&diff))
            .await
            .expect("publishing must succeed");

        let requests = received(&server).await;
        assert!(
            inline_posts(&requests).is_empty(),
            "a docs-only round posts no inline note: {requests:?}"
        );

        let board = board_body(&server).await;
        assert!(board.contains("Documentation/CI-only change"));
        assert!(
            board.contains("`AGENTS.md:1`"),
            "the board names the findings it withheld from inline delivery"
        );
        assert!(board.contains("`docs/design.md:1`"));
        assert_eq!(
            board.matches("### The document does not explain the workflow").count(),
            2,
            "the per-expert sections keep every finding"
        );
    }

    /// A mixed change set is not docs-only: the same findings are posted inline.
    #[tokio::test]
    async fn publish_review_posts_inline_notes_for_a_mixed_change_set() {
        let server = MockServer::start().await;
        mount_gitlab(&server).await;

        let files = ["AGENTS.md", "src/lib.rs"];
        let output = output_with(vec![
            finding("AGENTS.md", 1, "Docs issue"),
            finding("src/lib.rs", 1, "Code issue"),
        ]);
        let diff = diff_for(&files);

        crate::publish_review_with_diff("token", &mr_url(&server), &output, Some(&diff))
            .await
            .expect("publishing must succeed");

        let posted = inline_posts(&received(&server).await);
        assert_eq!(posted.len(), 2, "both findings are inside the diff and admitted");
        assert!(posted.iter().any(|body| body.contains("src/lib.rs")));
        assert!(posted.iter().any(|body| body.contains("AGENTS.md")));

        let board = board_body(&server).await;
        assert!(
            !board.contains("Documentation/CI-only change"),
            "a mixed change set is not summary-only"
        );
        assert!(
            !board.contains("## Inline notes — delivery policy"),
            "nothing was withheld, so the board gains no policy section"
        );
    }

    // ── RENG-99: the position GitLab accepts, and the note it refused ───────

    /// A one-hunk diff in which line 2 of each named file is **unchanged**
    /// context and line 3 is an **added** line — the two anchor shapes GitLab's
    /// position lookup distinguishes.
    fn diff_with_context_line(files: &[&str]) -> String {
        files
            .iter()
            .map(|file| {
                format!(
                    "diff --git a/{file} b/{file}\n\
                     index 1111111..2222222 100644\n\
                     --- a/{file}\n\
                     +++ b/{file}\n\
                     @@ -1,2 +1,3 @@\n\
                     \x20first\n\
                     \x20second\n\
                     +third\n"
                )
            })
            .collect()
    }

    /// The user-visible half of RENG-99, end to end: a refused inline note is
    /// written into the board — the report the user reads — with its count, its
    /// anchor and GitLab's verdict, instead of surviving only as a WARN. The
    /// note that *is* accepted on an unchanged line carries the old-side number
    /// its position needs.
    #[tokio::test]
    async fn publish_review_reports_a_refused_inline_note_in_the_board() {
        let server = MockServer::start().await;
        // Wiremock answers with the first *mounted* mock that matches (it sorts
        // by explicit priority only, and these are all equal), so the rejected
        // anchor has to be mounted before `mount_gitlab`'s catch-all discussions
        // handler — the same reason `tests/publish/main.rs` gives both of its
        // discussion mocks a body matcher.
        Mock::given(method("POST"))
            .and(path("/api/v4/projects/group%2Fproject/merge_requests/1/discussions"))
            .and(body_string_contains("bad.rs"))
            .respond_with(ResponseTemplate::new(400).set_body_string(
                r#"{"message":"400 Bad request - Note {:line_code=>[\"can't be blank\", \"must be a valid line code\"]}"}"#,
            ))
            .mount(&server)
            .await;
        mount_gitlab(&server).await;
        // The board update — the note created by `mount_gitlab` is id 7.
        Mock::given(method("PUT"))
            .and(path("/api/v4/projects/group%2Fproject/merge_requests/1/notes/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 7})))
            .mount(&server)
            .await;

        let files = ["good.rs", "bad.rs"];
        let output = output_with(vec![
            finding("good.rs", 2, "An issue on a line the change leaves unchanged"),
            finding("bad.rs", 3, "An issue on an added line"),
        ]);
        let diff = diff_with_context_line(&files);

        crate::publish_review_with_diff("token", &mr_url(&server), &output, Some(&diff))
            .await
            .expect_err("a refused inline note is reported to the caller");

        let requests = received(&server).await;

        // (a) The accepted note's position carries the old-side number of the
        // unchanged line, which is what GitLab matches on.
        let posted = inline_posts(&requests);
        let accepted = posted
            .iter()
            .find(|body| body.contains("good.rs"))
            .expect("the accepted note is posted");
        let accepted: serde_json::Value = serde_json::from_str(accepted).expect("the discussion body is JSON");
        assert_eq!(accepted["position"]["old_line"], 2);
        assert_eq!(accepted["position"]["new_line"], 2);
        assert!(
            accepted["position"].get("line_code").is_none(),
            "line_code is GitLab's to derive: {}",
            accepted["position"]
        );

        // (b) The refused note is reported in the board, not only in the log.
        let board_update = requests
            .iter()
            .find(|(method, path, _)| method == "PUT" && path.ends_with("/notes/7"))
            .map(|(_, _, body)| body.clone())
            .expect("the board must be updated with the publish outcome");
        assert!(
            board_update.contains("1 could not be published"),
            "the board states how many notes were lost: {board_update}"
        );
        assert!(
            board_update.contains("**1 inline note(s) failed to publish**"),
            "{board_update}"
        );
        assert!(
            board_update.contains("`bad.rs:3` — HTTP 400"),
            "the board names the refused anchor and GitLab's status: {board_update}"
        );
        assert!(
            board_update.contains("line_code"),
            "the board carries GitLab's verdict: {board_update}"
        );
        assert!(
            !board_update.contains("`good.rs:2` — HTTP"),
            "only the refused note is listed: {board_update}"
        );

        // The board itself (the POSTed body) is unchanged by the addendum: the
        // failure is a second, post-pass rendering of the same round.
        let initial_board = board_body(&server).await;
        assert!(
            !initial_board.contains("could not be published"),
            "the first board body cannot know the outcome yet: {initial_board}"
        );
    }

    /// A round that posts everything it admitted performs no board update at
    /// all — the RENG-99 addendum costs one request, and only when it has
    /// something to say.
    #[tokio::test]
    async fn publish_review_leaves_the_board_alone_when_nothing_was_refused() {
        let server = MockServer::start().await;
        mount_gitlab(&server).await;

        let files = ["good.rs"];
        let output = output_with(vec![finding("good.rs", 3, "An issue on an added line")]);
        let diff = diff_with_context_line(&files);

        crate::publish_review_with_diff("token", &mr_url(&server), &output, Some(&diff))
            .await
            .expect("publishing must succeed");

        let requests = received(&server).await;
        assert_eq!(inline_posts(&requests).len(), 1);
        assert!(
            !requests.iter().any(|(method, _, _)| method == "PUT"),
            "no failure, no addendum: {requests:?}"
        );
    }

    // ── RENG-113: the publish-time credential gate ─────────────────────────

    /// An inline note body that carries a plaintext password must NOT be
    /// posted: round-2 redacts `title` / `recommendation` inside
    /// [`format_inline_body`] itself, so the gate scans a body the
    /// renderer already masked. To exercise the gate's still-active
    /// defense-in-depth check, this test bypasses the redactor by handing
    /// the gate a *hand-built* body — the same shape `output_with`
    /// produces, which is what the round-1 test did. Round 2 keeps the
    /// refusal contract: even when a plaintext title somehow survives to
    /// the inline note (a renderer regression, a manual override), the
    /// gate still refuses, never posts.
    #[tokio::test]
    async fn publish_review_refuses_an_inline_note_with_a_plaintext_credential() {
        let server = MockServer::start().await;
        mount_gitlab(&server).await;

        // The `file` field is the only structural channel the inline note
        // body carries that the round-2 fix left unredacted (paths are
        // put-incredible by the inline-anchor gate). To exercise the
        // refusal contract with a *real* leak vector, route the plaintext
        // through `title` and use the hand-built renderer (so the redactor
        // never sees it).
        let f = finding("src/auth.rs", 3, "leaked: password = \"hunter2_real_secret\"");
        let output = output_with(vec![f]);
        let diff = diff_with_context_line(&["src/auth.rs"]);

        let err = crate::publish_review_with_diff("token", &mr_url(&server), &output, Some(&diff))
            .await
            .expect_err("a hand-built body whose inline note carries the plaintext must refuse");

        let msg = err.to_string();
        assert!(
            msg.contains("refusing to publish"),
            "the gate's first-line reason: {msg}"
        );
        assert!(
            !msg.contains("hunter2_real_secret"),
            "the value itself must never appear in the error: {msg}"
        );

        // Defence in depth: the gate ran before the provider saw the body,
        // so the mock server received neither a board POST nor an inline
        // POST.
        let requests = received(&server).await;
        assert!(
            requests.is_empty(),
            "no provider call may happen on a refused publish: {requests:?}"
        );
    }

    /// A finding whose title (rendered verbatim by the hand-built
    /// `output_with` fixture, which round 1 used to expose the leak) carries
    /// a credential triggers the same refusal. Round 2 closes the renderer
    /// channel too (the e2e tests above use the real renderer), so this
    /// is the negative half of round 2 — the gate stays live as a
    /// defense-in-depth check.
    #[tokio::test]
    async fn publish_review_refuses_a_board_with_a_plaintext_credential() {
        let server = MockServer::start().await;
        mount_gitlab(&server).await;

        let f = finding("src/auth.rs", 3, "leaked: password = \"hunter2_real_secret\"");
        let output = output_with(vec![f]);
        let diff = diff_with_context_line(&["src/auth.rs"]);

        let err = crate::publish_review_with_diff("token", &mr_url(&server), &output, Some(&diff))
            .await
            .expect_err("publishing must refuse a credential in the board");

        let msg = err.to_string();
        assert!(
            msg.contains("refusing to publish"),
            "the gate's first-line reason: {msg}"
        );
        assert!(
            msg.contains("board body"),
            "the gate names the offending surface: {msg}"
        );
        assert!(
            !msg.contains("hunter2_real_secret"),
            "the value itself must never appear in the error: {msg}"
        );

        let requests = received(&server).await;
        assert!(
            requests.is_empty(),
            "no provider call may happen on a refused publish: {requests:?}"
        );
    }

    /// A clean review still publishes: the gate must not be a false-positive
    /// time bomb. (The existing e2e tests already cover the canonical
    /// happy path; this one is the same shape with credentials absent.)
    #[tokio::test]
    async fn publish_review_passes_when_no_credentials_are_present() {
        let server = MockServer::start().await;
        mount_gitlab(&server).await;

        let output = output_with(vec![finding("src/auth.rs", 3, "Constant-time compare missing")]);
        let diff = diff_with_context_line(&["src/auth.rs"]);

        crate::publish_review_with_diff("token", &mr_url(&server), &output, Some(&diff))
            .await
            .expect("clean publish must succeed");
    }

    /// `format_publish_refusal` prints hit context (kind, key, len, fp)
    /// without echoing the value itself — the operator can grep the local
    /// report file for the fingerprint to find the offending line, but the
    /// error path itself stays credential-free.
    #[test]
    fn publish_refusal_error_names_context_without_the_value() {
        let body = "config.toml: password = \"hunter2_real_secret\"";
        let refusal = crate::models::scan_for_publish(body).unwrap_err();
        let msg = crate::format_publish_refusal(&refusal, "board");

        assert!(msg.contains("kind=password"));
        assert!(msg.contains("key=password"));
        assert!(msg.contains("len=19"), "the real length is 19 chars: {msg}");
        assert!(!msg.contains("hunter2_real_secret"), "the value is never echoed: {msg}");
        // The fingerprint is short (8 hex chars) and stable, so a reviewer
        // can grep the local report file for it.
        let fp_start = msg.find("fp=").expect("fp is in the error");
        let fp = &msg[fp_start + 3..fp_start + 3 + 8];
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()), "fp is hex: {fp}");
    }

    // ── RENG-113 round 2: the render→board→gate chain (P1 regression) ───

    /// RENG-113 round 2: a credential that lives in a field the renderer
    /// redacts (here `Finding::summary`) must NOT block `--publish` — the
    /// renderer is supposed to mask it, and the board that lands on the
    /// MR discussion must therefore carry the masked form. Before round 2
    /// the gate re-fired on its own marker and refused the post; this
    /// test pins the production call sequence (`render_expert_markdown` →
    /// `render_board` → `scan_for_publish`) so the regression cannot return
    /// through a renderer-rewrite that breaks the marker-aware scan.
    #[tokio::test]
    async fn publish_review_passes_through_a_masked_board_with_credential_in_summary() {
        let server = MockServer::start().await;
        mount_gitlab(&server).await;

        // The summary quotes a config line. The renderer masks it; the
        // gate must accept the masked board.
        let mut f = finding("src/auth.rs", 3, "Constant-time compare missing");
        f.summary = "the committed config line is password = \"hunter2_real_secret\" in config.toml.".to_string();
        f.evidence = "the line is password = \"hunter2_real_secret\"".to_string();
        let output = output_via_render(vec![f]);
        let diff = diff_with_context_line(&["src/auth.rs"]);

        crate::publish_review_with_diff("token", &mr_url(&server), &output, Some(&diff))
            .await
            .expect("a board whose renderer masked the credential must publish");

        // The board that landed on GitLab carries the marker, never the
        // plaintext — the same masked form the report file holds.
        let board = board_body(&server).await;
        assert!(
            !board.contains("hunter2_real_secret"),
            "the board body must not carry the plaintext: {board}"
        );
        assert!(board.contains("kind=password"), "the board carries the marker: {board}");
    }

    /// RENG-113 round 2: a credential that the renderer could not mask (the
    /// `title` field, before round 2) used to reach the board verbatim and
    /// trip the gate. After round 2 the title is also redacted — the gate
    /// still refuses, because the masked title's marker must NOT re-fire
    /// the scan (that's the round 2 fix). This test pins the "title is
    /// masked at render time, gate accepts the masked board" path.
    #[tokio::test]
    async fn publish_review_passes_through_a_masked_board_with_credential_in_title() {
        let server = MockServer::start().await;
        mount_gitlab(&server).await;

        // Title itself used to be the title channel that round 1 missed.
        // After round 2 it goes through `redact()` in both
        // `render_expert_markdown` and `render_team_report_with_scoring`.
        let mut f = finding("src/auth.rs", 3, "");
        f.title = "leaked: password = \"hunter2_real_secret\"".to_string();
        let output = output_via_render(vec![f]);
        let diff = diff_with_context_line(&["src/auth.rs"]);

        crate::publish_review_with_diff("token", &mr_url(&server), &output, Some(&diff))
            .await
            .expect("a board whose renderer masked the title must publish");

        let board = board_body(&server).await;
        assert!(
            !board.contains("hunter2_real_secret"),
            "the board body must not carry the plaintext title: {board}"
        );
    }

    /// RENG-113 round 2: a credential that lives in a channel the renderer
    /// does *not* mask (today `Finding::file`; a path is structural) must
    /// still be caught by the gate, so the publish refuses and the value
    /// never goes through. This is the negative half of the round-2 fix:
    /// the gate refuses on a real hit, even though the renderer has redacted
    /// everything it can see.
    #[tokio::test]
    async fn publish_review_still_refuses_when_credential_survives_every_redaction_path() {
        // Bypass the renderer entirely: hand the gate a board body that
        // contains a real password, even though the renderer would have
        // caught it. This is the same hand-built shape the round-1 tests
        // use — round 2 keeps the round-1 contract: a real plaintext hit
        // still refuses.
        let server = MockServer::start().await;
        mount_gitlab(&server).await;

        let f = finding("src/auth.rs", 3, "leaked: password = \"hunter2_real_secret\"");
        // Hand-build the markdown so the redactor never sees the title.
        // The board will carry the title verbatim; the gate must refuse.
        let output = output_with(vec![f]);
        let diff = diff_with_context_line(&["src/auth.rs"]);

        let err = crate::publish_review_with_diff("token", &mr_url(&server), &output, Some(&diff))
            .await
            .expect_err("a board whose renderer could not mask the credential must refuse");

        let msg = err.to_string();
        assert!(msg.contains("refusing to publish"), "{msg}");
        assert!(!msg.contains("hunter2_real_secret"), "the value is never echoed: {msg}");

        // Defence in depth: the gate ran before the provider saw the body.
        let requests = received(&server).await;
        assert!(
            requests.is_empty(),
            "no provider call may happen on a refused publish: {requests:?}"
        );
    }

    /// RENG-113 round 3: a credential in a *rolled-up* title (admitted
    /// by the policy but withheld from inline delivery past the per-round
    /// cap, default 2 → posts two notes inline, the rest go up the board)
    /// used to refuse the publish on its own rendered list — fail-closed,
    /// but a user-facing publish dead-end the operator couldn't resolve,
    /// and confusing because the per-expert section already masked the same
    /// title. Round 3 masks the rolled-up list in `board_section`, so the
    /// gate passes and the board carries the marker.
    #[tokio::test]
    async fn publish_review_passes_through_a_rolled_up_credential_title() {
        let server = MockServer::start().await;
        mount_gitlab(&server).await;

        // Three admitted findings, default cap is 2 → three goes to the
        // rolled-up list. The credential lives in that title.
        let files = ["src/f1.rs", "src/f2.rs", "src/f3.rs"];
        let mut findings: Vec<Finding> = files
            .iter()
            .enumerate()
            .map(|(i, file)| finding(file, 1, &format!("Issue {i}")))
            .collect();
        findings[2].title = "leaked: password = \"hunter2_real_secret\"".to_string();
        let output = output_via_render(findings);
        let diff = diff_for(&files);

        crate::publish_review_with_diff("token", &mr_url(&server), &output, Some(&diff))
            .await
            .expect("a board whose rolled-up list masked the credential must publish");

        // The board carries the marker, never the value.
        let board = board_body(&server).await;
        assert!(
            !board.contains("hunter2_real_secret"),
            "the rolled-up title must be masked: {board}"
        );
        assert!(
            board.contains("kind=password"),
            "the rolled-up marker is named in the board: {board}"
        );
        // The rolled-up list still names the anchor and the cap that
        // pulled the finding out of inline delivery — what the reviewer
        // expects to see.
        assert!(board.contains("`src/f3.rs:1`"), "the anchor is named: {board}");
        assert!(
            board.contains("exceeded the 2-note per-round cap"),
            "the cap explanation is named: {board}"
        );

        // Two inline notes went up (the first two), the third was the
        // rolled-up one. The inline notes' bodies are masked at source
        // (round-2 `format_inline_body`); the gate sees them masked too.
        let requests = received(&server).await;
        let posted = inline_posts(&requests);
        assert_eq!(posted.len(), 2, "the default cap is two notes per round: {requests:?}");
        // And the inline-note bodies are themselves masked — the
        // plaintext does not leak through the inline path either.
        for body in &posted {
            assert!(
                !body.contains("hunter2_real_secret"),
                "inline note body carries no plaintext: {body}"
            );
        }
    }
}
