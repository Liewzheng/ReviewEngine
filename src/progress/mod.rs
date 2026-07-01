//! Runtime progress tracking for multi-stage code reviews.
//!
//! Defines [`ProgressStatus`], [`StageProgress`], [`ReviewProgress`],
//! [`StageWeight`], and [`ProgressMap`] — a thread-safe, serialisable
//! progress-reporting system. Each review is broken into named stages
//! (e.g. "diff", "review", "aggregate"), each with a weight used to
//! compute an overall completion percentage. The [`ProgressMap`] is a
//! `RwLock<HashMap<String, ReviewProgress>>` shared across the application
//! so that API consumers can poll review progress in real time.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::RwLock;

// ─── Progress Status ──────────────────────────

/// Status of a single review stage or the overall review.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum ProgressStatus {
    /// Stage has not started yet.
    Pending,
    /// Stage is currently executing.
    Running,
    /// Stage completed successfully.
    Completed,
    /// Stage (or the whole review) failed with an error.
    Failed,
}

// ─── Stage Weight (static definition) ─────────

/// Defines a named review stage and its contribution to overall progress.
///
/// Weights are static fractions that sum to 1.0 across all stages.
/// Used by [`ReviewProgress::new`] to initialise progress tracking.
#[derive(Debug, Clone)]
pub struct StageWeight {
    /// Machine-readable stage name (e.g. `"parse"`, `"expert_review"`).
    pub name: &'static str,
    /// Human-readable label for display (e.g. `"Parsing diff"`).
    pub label: &'static str,
    /// Fraction of total progress this stage represents (sums to 1.0).
    pub weight: f64,
}

// ─── Stage Progress (runtime) ─────────────────

/// Runtime progress state for a single review stage.
#[derive(Debug, Clone, Serialize)]
pub struct StageProgress {
    /// Machine-readable stage name matching a [`StageWeight`].
    pub name: String,
    /// Human-readable label for display.
    pub label: String,
    /// Fraction of total progress this stage represents.
    pub weight: f64,
    /// Completion percentage for this stage (0.0–1.0).
    pub stage_percent: f64,
    /// Optional detail text describing the current sub-task.
    pub detail: String,
    /// Current status of this stage.
    pub status: ProgressStatus,
}

// ─── Overall Progress ─────────────────────────

/// Overall progress of a complete multi-stage review.
///
/// Tracks overall completion percentage, per-stage progress, and
/// timing information. Serializable for API consumption.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewProgress {
    /// Unique identifier for this review.
    pub review_id: String,
    /// Overall review status.
    pub status: ProgressStatus,
    /// Overall completion percentage (0.0–100.0).
    pub overall_percent: f64,
    /// Per-stage progress breakdown.
    pub stages: Vec<StageProgress>,
    /// ISO 8601 timestamp when the review started.
    pub created_at: String,
    /// ISO 8601 timestamp when the review completed (if done).
    pub completed_at: Option<String>,
    /// Error message if the review failed.
    pub error: Option<String>,
}

impl ReviewProgress {
    /// Create a new `ReviewProgress` for the given review and stage weights.
    ///
    /// All stages are initialised to `Pending` with 0% progress.
    pub fn new(review_id: String, stage_weights: &[StageWeight]) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            review_id,
            status: ProgressStatus::Running,
            overall_percent: 0.0,
            stages: stage_weights
                .iter()
                .map(|sw| StageProgress {
                    name: sw.name.to_string(),
                    label: sw.label.to_string(),
                    weight: sw.weight,
                    stage_percent: 0.0,
                    detail: String::new(),
                    status: ProgressStatus::Pending,
                })
                .collect(),
            created_at: now,
            completed_at: None,
            error: None,
        }
    }

    /// Update the progress of a named stage and recalculate overall progress.
    pub fn set_stage(&mut self, name: &str, percent: f64, detail: String) {
        if let Some(stage) = self.stages.iter_mut().find(|s| s.name == name) {
            stage.stage_percent = percent.clamp(0.0, 1.0);
            stage.detail = detail;
            stage.status = ProgressStatus::Running;
        }
        self.recalc_overall();
    }

    /// Mark a named stage as fully completed (100%).
    pub fn complete_stage(&mut self, name: &str) {
        if let Some(stage) = self.stages.iter_mut().find(|s| s.name == name) {
            stage.stage_percent = 1.0;
            stage.status = ProgressStatus::Completed;
        }
        self.recalc_overall();
    }

    /// Mark the entire review as failed with an error message.
    pub fn mark_failed(&mut self, error: String) {
        self.status = ProgressStatus::Failed;
        self.error = Some(error);
    }

    /// Mark the entire review as completed (all stages set to 100%).
    pub fn mark_completed(&mut self) {
        self.status = ProgressStatus::Completed;
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
        for stage in &mut self.stages {
            stage.status = ProgressStatus::Completed;
            stage.stage_percent = 1.0;
        }
        self.overall_percent = 100.0;
    }

    fn recalc_overall(&mut self) {
        let total: f64 = self.stages.iter().map(|s| s.weight * s.stage_percent).sum();
        self.overall_percent = (total * 100.0).clamp(0.0, 100.0);
    }
}

// ─── Global Registry ─────────────────────────

/// Thread-safe shared map of review ID to [`ReviewProgress`].
///
/// Cloned across async tasks to allow concurrent progress updates
/// and polling via the HTTP API.
pub type ProgressMap = std::sync::Arc<RwLock<HashMap<String, ReviewProgress>>>;

/// Create a new empty [`ProgressMap`].
pub fn new_progress_map() -> ProgressMap {
    std::sync::Arc::new(RwLock::new(HashMap::new()))
}

// ─── Stage Weight Definitions ────────────────

impl StageWeight {
    /// 小 PR（不走 chunk，全量 diff 直送专家）
    pub fn small_pr() -> Vec<StageWeight> {
        vec![
            StageWeight {
                name: "parse",
                label: "Parsing diff",
                weight: 0.05,
            },
            StageWeight {
                name: "expert_review",
                label: "Expert review",
                weight: 0.85,
            },
            StageWeight {
                name: "aggregate",
                label: "Aggregating reports",
                weight: 0.08,
            },
            StageWeight {
                name: "report",
                label: "Generating report",
                weight: 0.02,
            },
        ]
    }

    /// 大 PR（走压缩 → chunk → Pass 1 + Pass 2）
    pub fn large_pr() -> Vec<StageWeight> {
        vec![
            StageWeight {
                name: "parse",
                label: "Parsing & compressing",
                weight: 0.05,
            },
            StageWeight {
                name: "lead_overview",
                label: "Lead overview (Pass 1)",
                weight: 0.15,
            },
            StageWeight {
                name: "expert_review",
                label: "Expert review (Pass 2)",
                weight: 0.70,
            },
            StageWeight {
                name: "aggregate",
                label: "Aggregating reports",
                weight: 0.08,
            },
            StageWeight {
                name: "report",
                label: "Generating report",
                weight: 0.02,
            },
        ]
    }

    /// 全仓库审核
    pub fn repo_review() -> Vec<StageWeight> {
        vec![
            StageWeight {
                name: "scan",
                label: "Scanning repository",
                weight: 0.05,
            },
            StageWeight {
                name: "local_analysis",
                label: "Local analysis",
                weight: 0.05,
            },
            StageWeight {
                name: "llm_enhance",
                label: "LLM enhancement",
                weight: 0.85,
            },
            StageWeight {
                name: "report",
                label: "Generating report",
                weight: 0.05,
            },
        ]
    }
}

// ─── Progress Update Helpers ────────────────

/// Update progress after an expert completes. Uses an atomic counter for monotonicity.
pub fn update_expert_progress(
    progress_map: Option<&ProgressMap>,
    review_id: &str,
    completed: &std::sync::atomic::AtomicUsize,
    total_tasks: usize,
) {
    if let Some(ref map) = progress_map {
        if let Ok(mut p) = map.write() {
            if let Some(progress) = p.get_mut(review_id) {
                let done = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                let total = total_tasks as f64;
                progress.set_stage(
                    "expert_review",
                    done as f64 / total,
                    format!("{}/{} tasks done", done, total_tasks),
                );
            }
        }
    }
}

/// Complete the expert_review, aggregate, and report stages and mark the review as completed.
pub fn complete_progress(progress_map: Option<&ProgressMap>, review_id: &str) {
    if let Some(ref map) = progress_map {
        if let Ok(mut p) = map.write() {
            if let Some(progress) = p.get_mut(review_id) {
                progress.complete_stage("expert_review");
                progress.complete_stage("aggregate");
                progress.complete_stage("report");
                progress.mark_completed();
            }
        }
    }
}

/// Complete the report stage only (not overall).
pub fn mark_report_complete(progress_map: Option<&ProgressMap>, review_id: &str) {
    if let Some(ref map) = progress_map {
        if let Ok(mut p) = map.write() {
            if let Some(progress) = p.get_mut(review_id) {
                progress.complete_stage("report");
            }
        }
    }
}

/// Complete the aggregate stage only.
pub fn mark_aggregate_complete(progress_map: Option<&ProgressMap>, review_id: &str) {
    if let Some(ref map) = progress_map {
        if let Ok(mut p) = map.write() {
            if let Some(progress) = p.get_mut(review_id) {
                progress.complete_stage("aggregate");
            }
        }
    }
}

/// Complete progress for repo review (stages: scan, local_analysis, llm_enhance, report).
pub fn complete_repo_progress(progress_map: Option<&ProgressMap>, review_id: &str) {
    if let Some(ref map) = progress_map {
        if let Ok(mut p) = map.write() {
            if let Some(progress) = p.get_mut(review_id) {
                progress.complete_stage("report");
                progress.mark_completed();
            }
        }
    }
}
