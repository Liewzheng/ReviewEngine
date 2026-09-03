use crate::server::api::types::{
    ExpertResultDetail, ReviewDetail, ReviewDetailAuthor, ReviewListItem, ReviewSource, TaskStatus,
};
use crate::server::task_queue::{SourceMeta, TaskEntry, TaskState};

pub(crate) fn task_status_str(state: &TaskState) -> &'static str {
    match state {
        TaskState::Pending => "pending",
        TaskState::Running => "running",
        TaskState::Completed => "completed",
        TaskState::Failed => "failed",
        TaskState::Cancelled => "cancelled",
    }
}

pub(crate) fn merge_camel_case_fields(base: &mut serde_json::Value, extra: &serde_json::Value) {
    if let (Some(base_obj), Some(extra_obj)) = (base.as_object_mut(), extra.as_object()) {
        for (k, v) in extra_obj {
            base_obj.insert(k.clone(), v.clone());
        }
    }
}

pub(crate) fn task_to_status(entry: &TaskEntry) -> TaskStatus {
    let meta = &entry.source_meta;
    TaskStatus {
        task_id: entry.task_id,
        status: task_status_str(&entry.state),
        created_at: entry.created_at.to_rfc3339(),
        completed_at: entry.completed_at.map(|t| t.to_rfc3339()),
        duration_ms: entry.duration_ms(),
        result: entry.result.clone(),
        error: entry.error.clone(),
        mr_title: meta.mr_title.clone(),
        project: meta.project.clone(),
        repository: meta.repository.clone(),
        branch: meta.branch.clone(),
        target_branch: meta.target_branch.clone(),
        author_name: meta.author_name.clone(),
        author_avatar_url: meta.author_avatar_url.clone(),
        gitlab_mr_url: meta.gitlab_mr_url.clone(),
        commit_sha: meta.commit_sha.clone(),
        progress: entry.progress,
        expert_name: entry.expert_name.clone(),
    }
}

pub(crate) fn build_review_detail(entry: &TaskEntry) -> ReviewDetail {
    let meta = &entry.source_meta;
    let status = task_status_str(&entry.state);

    let (experts, raw_comment) = match &entry.result {
        Some(result) => match serde_json::from_value::<crate::models::ReviewOutput>(result.clone()) {
            Ok(output) => {
                let experts = output
                    .reports
                    .iter()
                    .map(|report| ExpertResultDetail {
                        expert_id: report.expert_name.clone(),
                        expert_name: report.expert_name.clone(),
                        status: "success".to_string(),
                        score: Some(crate::scoring::review::expert_score(&report.findings)),
                        summary: if report.markdown.is_empty() {
                            format!("{} finding(s)", report.findings.len())
                        } else {
                            report.markdown.clone()
                        },
                        details: if report.raw_llm_response.is_empty() {
                            None
                        } else {
                            Some(report.raw_llm_response.clone())
                        },
                    })
                    .collect();
                let raw_comment = output
                    .aggregated
                    .as_ref()
                    .map(|agg| agg.markdown.clone())
                    // §8.3: no aggregator output → fall back to the lead
                    // consolidation TL;DR so the "full comment" tab is not
                    // empty; empty strings degrade to None (empty state).
                    .or_else(|| output.consolidated.as_ref().map(|c| c.assessment.tl_dr.clone()))
                    .filter(|s| !s.is_empty());
                (experts, raw_comment)
            }
            Err(_) => (Vec::new(), None),
        },
        None => (Vec::new(), None),
    };

    ReviewDetail {
        id: entry.task_id.to_string(),
        mr_title: meta.mr_title.clone(),
        project: meta.project.clone(),
        repository: meta.repository.clone(),
        branch: meta.branch.clone(),
        target_branch: meta.target_branch.clone(),
        author: ReviewDetailAuthor {
            name: meta.author_name.clone(),
            avatar_url: meta.author_avatar_url.clone(),
        },
        status: status.to_string(),
        duration_ms: entry.duration_ms(),
        created_at: entry.created_at.to_rfc3339(),
        completed_at: entry.completed_at.map(|t| t.to_rfc3339()),
        commit_sha: meta.commit_sha.clone(),
        experts,
        raw_comment,
        raw_api_response: entry.result.clone(),
        gitlab_mr_url: meta.gitlab_mr_url.clone(),
    }
}

pub(crate) fn build_review_list_item(entry: &TaskEntry) -> ReviewListItem {
    let meta = &entry.source_meta;
    ReviewListItem {
        id: entry.task_id.to_string(),
        mr_title: meta.mr_title.clone(),
        project: meta.project.clone(),
        repository: meta.repository.clone(),
        branch: meta.branch.clone(),
        target_branch: meta.target_branch.clone(),
        author: ReviewDetailAuthor {
            name: meta.author_name.clone(),
            avatar_url: meta.author_avatar_url.clone(),
        },
        status: task_status_str(&entry.state).to_string(),
        duration_ms: entry.duration_ms(),
        created_at: entry.created_at.to_rfc3339(),
        gitlab_mr_url: meta.gitlab_mr_url.clone(),
    }
}

pub(crate) fn source_meta_from_request(source: &ReviewSource) -> SourceMeta {
    match source {
        ReviewSource::GitLabMr { url, .. } => {
            let mut meta = SourceMeta::default();
            if let Some((path_part, _)) = url.split_once("/-/merge_requests/") {
                if let Some((_proto, rest)) = path_part.split_once("://") {
                    if let Some((_, path)) = rest.split_once('/') {
                        meta.project = Some(path.to_string());
                        meta.repository = Some(path.to_string());
                        meta.gitlab_mr_url = Some(url.clone());
                    }
                }
            }
            meta
        }
        ReviewSource::LocalRepo { path, .. } => SourceMeta {
            project: Some(path.clone()),
            repository: Some(path.clone()),
            ..SourceMeta::default()
        },
        ReviewSource::StaticDiff { .. } => SourceMeta::default(),
    }
}

use crate::server::task_queue::{source_meta_from_mr_info, TaskStore};
use crate::server::AppState;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct ListParams {
    pub status: Option<String>,
    pub page: Option<u64>,
    pub per_page: Option<u64>,
    pub q: Option<String>,
    pub project: Option<String>,
    pub repository: Option<String>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
}

pub(crate) async fn enqueue_review(
    state: &Arc<AppState>,
    store: &TaskStore,
    request: crate::server::api::types::ReviewRequest,
    request_json: serde_json::Value,
    gitlab_token: Option<String>,
) -> uuid::Uuid {
    let source_meta = source_meta_from_request(&request.source);
    // `request_json` is serialized from the credential-free `ReviewRequest`
    // struct: the GitLab token travels only in the `gitlab_token` parameter
    // (resolved from the X-Gitlab-Token header / server config) and is never
    // persisted into the task store, so rerun re-resolves it at rerun time.
    let task_id = store.create_with_request(Some(source_meta), Some(request_json)).await;
    let store_clone = store.clone();
    let source = request.source;
    let config_toml = request.config;
    let llm_configs = match request.llm_configs {
        Some(configs) if !configs.is_empty() => configs,
        _ => state.llm_configs.read().unwrap().clone(),
    };
    let webhook = request.webhook;
    let cfg = state.app_config.read().unwrap().clone();

    // 0.10.0 §7.2: pre-review discussion tap, GitLab MR sources only. Built
    // synchronously before the spawn so the git_platforms RwLock guard never
    // crosses an .await. `None` when no DB is wired → exact 0.9 behaviour.
    let (mr_url, tap) = match &source {
        ReviewSource::GitLabMr { url } => {
            let tap = state.db.clone().map(|db| {
                let platform = {
                    let platforms = state.git_platforms.read().unwrap();
                    crate::models::find_git_platform_for_url_strict(&platforms, url).cloned()
                };
                super::discussion::DiscussionTap::new(db, platform.as_ref(), url)
            });
            (Some(url.clone()), tap)
        }
        _ => (None, None),
    };
    let token_for_tap = gitlab_token.clone();

    tokio::spawn(async move {
        while !store_clone.can_start_new_task().await {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        store_clone.update(task_id, TaskState::Running, None, None).await;

        let task_started = std::time::Instant::now();
        crate::server::log_collector::push_global_entry(
            "INFO",
            format!("Review task {} started", task_id),
            Some(crate::server::log_collector::LogMetadata {
                request_id: Some(task_id.to_string()),
                duration_ms: None,
                review_id: Some(task_id.to_string()),
                expert_id: None,
            }),
        );

        // Resolve the source first: for gitlab_mr reviews this fetches the MR
        // metadata up front, so the task record is back-filled (History shows
        // the real title/branch/author/commit) even when the review itself
        // later fails. Fill happens before the (possibly long) expert run and
        // only touches fields still blank, so enqueue-time values win.
        let outcome = match super::resolve::resolve_source(source, gitlab_token, &cfg).await {
            Ok(mut resolved) => {
                if let Some(ref mut info) = resolved.mr_info {
                    store_clone
                        .fill_source_meta(task_id, source_meta_from_mr_info(info))
                        .await;
                    // §7.2: inject the MR discussion history into the prompt
                    // context. Best-effort — any failure degrades to `None`
                    // and the review runs with the 0.9 prompt.
                    if let (Some(tap), Some(url), Some(token)) =
                        (tap.as_ref(), mr_url.as_deref(), token_for_tap.as_deref())
                    {
                        if let Some(section) = tap
                            .inject(task_id, &info.project_path, u64::from(info.mr_iid), token, url)
                            .await
                        {
                            info.discussion_context = Some(section);
                        }
                    }
                }
                super::resolve::run_review(resolved, config_toml, llm_configs).await
            }
            Err(e) => Err(e),
        };

        match outcome {
            Ok((value, summary)) => {
                crate::server::log_collector::push_global_entry(
                    "INFO",
                    format!("Review task {} completed: {}", task_id, summary),
                    Some(crate::server::log_collector::LogMetadata {
                        request_id: Some(task_id.to_string()),
                        duration_ms: Some(task_started.elapsed().as_millis() as u64),
                        review_id: Some(task_id.to_string()),
                        expert_id: None,
                    }),
                );
                store_clone
                    .update(task_id, TaskState::Completed, Some(value), None)
                    .await;
                crate::server::api::callback::spawn_callback(webhook, task_id, "completed", Some(summary), None);
            }
            Err(e) => {
                let message = e.to_string();
                crate::server::log_collector::push_global_entry(
                    "ERROR",
                    format!("Review task {} failed: {}", task_id, message),
                    Some(crate::server::log_collector::LogMetadata {
                        request_id: Some(task_id.to_string()),
                        duration_ms: Some(task_started.elapsed().as_millis() as u64),
                        review_id: Some(task_id.to_string()),
                        expert_id: None,
                    }),
                );
                store_clone
                    .update(task_id, TaskState::Failed, None, Some(message.clone()))
                    .await;
                crate::server::api::callback::spawn_callback(webhook, task_id, "failed", None, Some(message));
            }
        }
    });
    task_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AggregatedReport, OverallAssessment, ReviewOutput, RiskLevel};
    use crate::server::task_queue::SourceMeta;
    use crate::team::lead_consolidator::ConsolidatedReport;

    fn entry_with_result(result: Option<serde_json::Value>) -> TaskEntry {
        TaskEntry {
            task_id: uuid::Uuid::new_v4(),
            state: TaskState::Completed,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            result,
            error: None,
            request: None,
            source_meta: SourceMeta::default(),
            progress: None,
            expert_name: None,
        }
    }

    fn consolidated_with_tl_dr(tl_dr: &str) -> ConsolidatedReport {
        ConsolidatedReport {
            findings: Vec::new(),
            low_confidence_removed: 0,
            duplicates_merged: 0,
            conflicts: Vec::new(),
            assessment: OverallAssessment {
                score: 80,
                risk_level: RiskLevel::Low,
                lead_override: None,
                tl_dr: tl_dr.to_string(),
                unverified: false,
                coverage_insufficient: false,
            },
            consensus_reached: true,
            total_files: 0,
            reviewed_files: 0,
            unreviewed_files: Vec::new(),
            coverage: None,
            adjudicated_removed: Vec::new(),
        }
    }

    fn aggregated_with_markdown(markdown: &str) -> AggregatedReport {
        AggregatedReport {
            findings: Vec::new(),
            markdown: markdown.to_string(),
            raw_llm_response: String::new(),
            parse_error: None,
            raw_dump_path: None,
        }
    }

    /// §8.3 regression: the aggregator markdown wins when both are present.
    #[test]
    fn raw_comment_prefers_aggregated_markdown() {
        let mut output = ReviewOutput::new(Vec::new());
        output.aggregated = Some(aggregated_with_markdown("# Aggregated"));
        output.consolidated = Some(consolidated_with_tl_dr("tldr"));
        let entry = entry_with_result(Some(serde_json::to_value(output).unwrap()));

        let detail = build_review_detail(&entry);
        assert_eq!(detail.raw_comment.as_deref(), Some("# Aggregated"));
    }

    /// §8.3: no aggregator output → the consolidation TL;DR fills the
    /// "full comment" tab.
    #[test]
    fn raw_comment_falls_back_to_consolidated_tl_dr() {
        let mut output = ReviewOutput::new(Vec::new());
        output.consolidated = Some(consolidated_with_tl_dr("TL;DR: looks fine"));
        let entry = entry_with_result(Some(serde_json::to_value(output).unwrap()));

        let detail = build_review_detail(&entry);
        assert_eq!(detail.raw_comment.as_deref(), Some("TL;DR: looks fine"));
    }

    /// §8.3: neither source present (or only empty strings) → None, the
    /// pre-fix empty-state semantics.
    #[test]
    fn raw_comment_none_when_nothing_to_show() {
        // Both absent.
        let output = ReviewOutput::new(Vec::new());
        let entry = entry_with_result(Some(serde_json::to_value(output).unwrap()));
        assert!(build_review_detail(&entry).raw_comment.is_none());

        // Present but empty → filtered out, same empty state.
        let mut output = ReviewOutput::new(Vec::new());
        output.aggregated = Some(aggregated_with_markdown(""));
        output.consolidated = Some(consolidated_with_tl_dr(""));
        let entry = entry_with_result(Some(serde_json::to_value(output).unwrap()));
        assert!(build_review_detail(&entry).raw_comment.is_none());

        // No parseable result at all.
        let entry = entry_with_result(None);
        assert!(build_review_detail(&entry).raw_comment.is_none());
    }
}
