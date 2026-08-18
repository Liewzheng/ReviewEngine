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
/// wins; when absent/blank the server-side configured token is used — the
/// GitLab runtime config seeded at startup from `--gitlab-token` /
/// `GITLAB_TOKEN` and mutable via `PUT /api/v1/config`. Returns `None` when
/// neither source yields a token; callers turn that into a `400`.
pub(crate) fn resolve_gitlab_token(header: Option<&str>) -> Option<String> {
    if let Some(t) = header.map(str::trim).filter(|t| !t.is_empty()) {
        return Some(t.to_string());
    }
    crate::server::gitlab::gitlab_runtime()
        .read()
        .ok()
        .map(|rt| rt.token.trim().to_string())
        .filter(|t| !t.is_empty())
}

pub(crate) async fn run_review(
    source: ReviewSource,
    gitlab_token: Option<String>,
    cfg: &Option<Arc<crate::models::AppConfig>>,
    config_toml: Option<String>,
    llm_configs: Vec<crate::models::LLMConfig>,
) -> anyhow::Result<(serde_json::Value, String)> {
    let diff_raw = resolve_source(source, gitlab_token, cfg).await?;

    let config_source = config_toml.map(crate::models::ConfigSource::Inline);
    let app_config = crate::config::resolve_config(config_source).await?;

    let experts = app_config.build_expert_defs();
    let mr_info = crate::models::MRInfo::new(
        "api".to_string(),
        "API Review".to_string(),
        "unknown".to_string(),
        "unknown".to_string(),
    );

    let review_result = tokio::time::timeout(
        std::time::Duration::from_secs(600),
        orchestrator::run_experts(&experts, &mr_info, &diff_raw, &llm_configs, &app_config, None, "", None),
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
) -> anyhow::Result<String> {
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
            let diff = client.fetch_diff().await?;
            Ok(diff)
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
            Ok(diff)
        }
        ReviewSource::StaticDiff { diff } => {
            if diff.len() > MAX_STATIC_DIFF_BYTES {
                anyhow::bail!(
                    "Static diff exceeds maximum size of {} MB",
                    MAX_STATIC_DIFF_BYTES / (1024 * 1024)
                );
            }
            Ok(diff)
        }
    }
}
