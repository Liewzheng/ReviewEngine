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
/// wins; when absent/blank, a configured git platform whose `base_url`
/// scheme-less `host[:port]` matches the MR URL supplies the token for that
/// instance; when no platform matches (or the match has no token), the
/// legacy server-side token is used — the GitLab runtime config seeded at
/// startup from `--gitlab-token` / `GITLAB_TOKEN` and mutable via
/// `PUT /api/v1/config`. Returns `None` when no source yields a token;
/// callers turn that into a `400`.
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
        // Strict match only: the resolved token is SENT to the MR URL's
        // host:port, so it must never flow to a port that was not explicitly
        // configured (unlike inbound webhook verification, which folds a
        // uniquely-matched host — see find_git_platform_for_url).
        if let Some(platform) = crate::models::find_git_platform_for_url_strict(platforms, url) {
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
#[derive(Debug)]
pub(crate) struct ResolvedSource {
    pub diff: String,
    pub mr_info: Option<crate::models::MRInfo>,
}

pub(crate) async fn run_review(
    resolved: ResolvedSource,
    config_toml: Option<String>,
    llm_configs: Vec<crate::models::LLMConfig>,
) -> anyhow::Result<(serde_json::Value, String)> {
    let config_source = config_toml.map(crate::models::ConfigSource::Inline);
    let app_config = crate::config::resolve_config(config_source).await?;

    let experts = app_config.build_expert_defs();
    // MR-based reviews reuse the freshly fetched metadata so prompts carry the
    // real title/branches; local/static sources keep the placeholder context.
    let mr_info = resolved.mr_info.unwrap_or_else(|| {
        crate::models::MRInfo::new(
            "api".to_string(),
            "API Review".to_string(),
            "unknown".to_string(),
            "unknown".to_string(),
        )
    });

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
            Ok(ResolvedSource {
                diff,
                mr_info: Some(mr_info),
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
            Ok(ResolvedSource { diff, mr_info: None })
        }
        ReviewSource::StaticDiff { diff } => {
            if diff.len() > MAX_STATIC_DIFF_BYTES {
                anyhow::bail!(
                    "Static diff exceeds maximum size of {} MB",
                    MAX_STATIC_DIFF_BYTES / (1024 * 1024)
                );
            }
            Ok(ResolvedSource { diff, mr_info: None })
        }
    }
}
