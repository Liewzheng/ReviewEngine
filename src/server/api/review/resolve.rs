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
) -> anyhow::Result<(serde_json::Value, String)> {
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

    let (reports, _, dropped_findings, consolidated) = match review_result {
        Ok(result) => result?,
        Err(_) => anyhow::bail!("Task timed out after 600 seconds"),
    };

    let output = crate::models::ReviewOutput::new(reports)
        .with_dropped_findings(dropped_findings)
        .with_consolidated(consolidated);
    let findings: usize = output.reports.iter().map(|r| r.findings.len()).sum();
    let summary = format!("{} expert report(s), {} finding(s)", output.reports.len(), findings);
    let value = serde_json::to_value(&output).unwrap_or_default();
    Ok((value, summary))
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
        let (value, summary) = run_review(
            resolved,
            Some(INLINE.to_string()),
            vec![],
            // RENG-57: no store in this test → no latency samples to record.
            None,
            Arc::new(overrides),
        )
        .await
        .expect("an empty expert team is a legitimate (empty) review");

        assert_eq!(
            value["reports"].as_array().map(Vec::len),
            Some(0),
            "every expert was disabled by the override: {value}"
        );
        assert_eq!(summary, "0 expert report(s), 0 finding(s)");
    }
}
