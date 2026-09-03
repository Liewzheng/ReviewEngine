use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Lifecycle state of a review task in the queue.
///
/// Transitions: `Pending` → `Running` → (`Completed` | `Failed`).
/// `Cancelled` is terminal; once set, the worker's `update` call is a no-op.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskState {
    /// Task is queued but not yet started (no worker has picked it up).
    Pending,
    /// A worker has claimed the task and is actively executing the review.
    Running,
    /// Review completed successfully; `result` contains the JSON report.
    Completed,
    /// Review failed; `error` contains the failure message.
    Failed,
    /// Task was cancelled by the user before or during execution.
    Cancelled,
}

/// Metadata about the source merge request or pull request.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SourceMeta {
    pub mr_title: Option<String>,
    pub project: Option<String>,
    pub repository: Option<String>,
    pub branch: Option<String>,
    pub target_branch: Option<String>,
    pub author_name: Option<String>,
    pub author_avatar_url: Option<String>,
    pub gitlab_mr_url: Option<String>,
    pub commit_sha: Option<String>,
}

/// Map the MR metadata resolved by the review pipeline (`MRInfo`, fetched
/// from the provider API) onto task source metadata for the History UI.
/// Empty strings map to `None` — a missing value must stay absent rather
/// than be persisted as `""`. Paired with [`TaskStore::fill_source_meta`]'s
/// fill-only-blank semantics so enqueue-time values are never clobbered.
pub(crate) fn source_meta_from_mr_info(info: &crate::models::MRInfo) -> SourceMeta {
    fn non_empty(s: &str) -> Option<String> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
    SourceMeta {
        mr_title: non_empty(&info.title),
        branch: non_empty(&info.source_branch),
        target_branch: non_empty(&info.target_branch),
        author_name: info.pr_author.clone(),
        commit_sha: non_empty(&info.git_hash),
        ..SourceMeta::default()
    }
}

/// A single review task record stored in the queue.
///
/// Created when a review request arrives; mutated as the task progresses
/// through `Pending` → `Running` → terminal state. Expired entries
/// (completed >30 min ago) are reaped automatically.
#[derive(Debug, Clone)]
pub struct TaskEntry {
    /// Unique task identifier (UUID v4).
    pub task_id: Uuid,
    /// Current lifecycle state.
    pub state: TaskState,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    /// Original review request parameters (serialized `ReviewRequest`),
    /// retained so the task can be re-run with identical inputs.
    pub request: Option<serde_json::Value>,
    pub source_meta: SourceMeta,
    pub progress: Option<u8>,        // 0-100
    pub expert_name: Option<String>, // current active expert
}

/// A real-time event broadcast to SSE subscribers when a task's state changes.
///
/// Broadcast on every state transition. The frontend's queue monitor
/// listens to these to update its live dashboard.
#[derive(Debug, Clone)]
pub struct TaskEvent {
    /// Task UUID this event pertains to.
    pub task_id: Uuid,
    /// Current status string: `"pending"`, `"running"`, `"completed"`, `"failed"`, `"cancelled"`.
    pub status: &'static str,
    /// Event type: `"review.created"`, `"review.started"`, `"review.progress"`, etc.
    pub event: &'static str,
    /// MR/PR title for display in the dashboard.
    pub mr_title: Option<String>,
    /// Project or namespace path.
    pub project: Option<String>,
    /// Completion percentage (0–100, only for `review.progress` events).
    pub progress: Option<u8>,
    /// Name of the expert currently executing (for progress events).
    pub expert_name: Option<String>,
    /// Elapsed wall-clock milliseconds since task started.
    pub elapsed_ms: Option<u64>,
}

/// In-memory task queue backed by `HashMap<Uuid, TaskEntry>`.
///
/// Provides concurrency-safe task lifecycle management with automatic
/// expiry cleanup, broadcast SSE events, pause/resume control, and
/// configurable concurrency limits.
///
/// Persistence (0.10.0, design/persistence.md §5): when `db` is injected via
/// [`set_db`](Self::set_db), every lifecycle transition is ALSO written
/// through to the database, synchronously awaited (restart-recovery
/// semantics rely on the DB being current). Write failures never block the
/// review path — they are logged; terminal writes are retried once. `None`
/// (the default) is exactly the 0.9 pure in-memory behaviour, which the
/// runtime-free sync unit tests depend on.
#[derive(Clone)]
pub struct TaskStore {
    inner: Arc<RwLock<HashMap<Uuid, TaskEntry>>>,
    tx: tokio::sync::broadcast::Sender<TaskEvent>,
    is_paused: Arc<RwLock<bool>>,
    max_concurrent: Arc<RwLock<usize>>,
    queue_capacity: Arc<RwLock<usize>>,
    db: Option<Arc<dyn crate::store::traits::ReviewStore>>,
}

impl TaskStore {
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(256);
        let inner: Arc<RwLock<HashMap<Uuid, TaskEntry>>> = Arc::new(RwLock::new(HashMap::new()));

        // Only start the background reaper when a Tokio runtime is present.
        // `TaskStore::new()` is also reached from `AppState::new()`, which sync
        // unit tests construct without a runtime — `tokio::spawn` would panic
        // there. The store is fully functional without the reaper (completed
        // entries just aren't auto-reaped); `cleanup_expired()` covers the
        // manual path.
        if tokio::runtime::Handle::try_current().is_ok() {
            let cleanup_inner = Arc::clone(&inner);
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
                loop {
                    interval.tick().await;
                    let cutoff = chrono::Utc::now() - chrono::Duration::minutes(30);
                    let mut map = cleanup_inner.write().await;
                    map.retain(|_, entry| match entry.completed_at {
                        Some(t) if t < cutoff => false,
                        _ => true,
                    });
                }
            });
        }

        Self {
            inner,
            tx,
            is_paused: Arc::new(RwLock::new(false)),
            max_concurrent: Arc::new(RwLock::new(8)),
            queue_capacity: Arc::new(RwLock::new(16)),
            db: None,
        }
    }

    /// Inject the write-through persistence target (0.10.0 §5.2). Call before
    /// the store is shared with workers; leaving it unset keeps the 0.9 pure
    /// in-memory behaviour.
    pub fn set_db(&mut self, db: Arc<dyn crate::store::traits::ReviewStore>) {
        self.db = Some(db);
    }

    pub async fn cleanup_expired(&self) {
        let cutoff = chrono::Utc::now() - chrono::Duration::minutes(30);
        let mut map = self.inner.write().await;
        map.retain(|_, entry| match entry.completed_at {
            Some(t) if t < cutoff => false,
            _ => true,
        });
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TaskEvent> {
        self.tx.subscribe()
    }

    pub async fn create(&self, source_meta: Option<SourceMeta>) -> Uuid {
        self.create_with_request(source_meta, None).await
    }

    /// Create a task, optionally retaining the serialized request parameters so
    /// it can later be re-run (`POST /reviews/{task_id}/rerun`).
    pub async fn create_with_request(
        &self,
        source_meta: Option<SourceMeta>,
        request: Option<serde_json::Value>,
    ) -> Uuid {
        let id = Uuid::new_v4();
        let entry = TaskEntry {
            task_id: id,
            state: TaskState::Pending,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            result: None,
            error: None,
            request,
            source_meta: source_meta.unwrap_or_default(),
            progress: None,
            expert_name: None,
        };
        self.inner.write().await.insert(id, entry.clone());
        let _ = self.tx.send(TaskEvent {
            task_id: id,
            status: "pending",
            event: "review.created",
            mr_title: None,
            project: None,
            progress: None,
            expert_name: None,
            elapsed_ms: None,
        });
        // Write-through (§5.2): INSERT reviews (state=pending).
        if let Some(db) = &self.db {
            if let Err(e) = db.create(&entry).await {
                tracing::error!("failed to persist new review task {id} to the database: {e:#}");
            }
        }
        id
    }

    pub async fn start(&self, task_id: Uuid) {
        let mut started_at = None;
        if let Some(entry) = self.inner.write().await.get_mut(&task_id) {
            let now = chrono::Utc::now();
            entry.state = TaskState::Running;
            entry.started_at = Some(now);
            started_at = Some(now);
            let _ = self.tx.send(TaskEvent {
                task_id,
                status: "running",
                event: "review.started",
                mr_title: entry.source_meta.mr_title.clone(),
                project: entry.source_meta.project.clone(),
                progress: None,
                expert_name: None,
                elapsed_ms: None,
            });
        }
        // Write-through (§5.2): UPDATE state=running, started_at. Awaited
        // AFTER the in-memory write lock is released.
        if let (Some(db), Some(at)) = (&self.db, started_at) {
            if let Err(e) = db.mark_started(task_id, at).await {
                tracing::error!("failed to persist review start for task {task_id}: {e:#}");
            }
        }
    }

    pub async fn set_progress(&self, task_id: Uuid, progress: u8, expert_name: Option<String>) {
        if let Some(entry) = self.inner.write().await.get_mut(&task_id) {
            entry.progress = Some(progress.min(100));
            entry.expert_name = expert_name.clone();
            let elapsed = entry
                .started_at
                .map(|s| (chrono::Utc::now() - s).num_milliseconds() as u64);
            let _ = self.tx.send(TaskEvent {
                task_id,
                status: "running",
                event: "review.progress",
                mr_title: entry.source_meta.mr_title.clone(),
                project: entry.source_meta.project.clone(),
                progress: Some(progress.min(100)),
                expert_name,
                elapsed_ms: elapsed,
            });
        }
    }

    pub async fn update(
        &self,
        task_id: Uuid,
        new_state: TaskState,
        result: Option<serde_json::Value>,
        error: Option<String>,
    ) {
        let mut terminal_snapshot = None;
        if let Some(entry) = self.inner.write().await.get_mut(&task_id) {
            // Cancelled is terminal: a background task racing past a DELETE
            // must not flip the record back to running/completed.
            if entry.state == TaskState::Cancelled {
                return;
            }
            entry.state = new_state.clone();
            entry.result = result;
            entry.error = error.clone();
            if new_state == TaskState::Completed || new_state == TaskState::Failed || new_state == TaskState::Cancelled
            {
                entry.completed_at = Some(chrono::Utc::now());
                terminal_snapshot = Some(entry.clone());
            }
            let event = match new_state {
                TaskState::Pending => "review.created",
                TaskState::Running => "review.started",
                TaskState::Completed => "review.completed",
                TaskState::Failed => "review.failed",
                TaskState::Cancelled => "review.cancelled",
            };
            let status = match new_state {
                TaskState::Pending => "pending",
                TaskState::Running => "running",
                TaskState::Completed => "completed",
                TaskState::Failed => "failed",
                TaskState::Cancelled => "cancelled",
            };
            let elapsed = entry
                .started_at
                .map(|s| (chrono::Utc::now() - s).num_milliseconds() as u64);
            let _ = self.tx.send(TaskEvent {
                task_id,
                status,
                event,
                mr_title: entry.source_meta.mr_title.clone(),
                project: entry.source_meta.project.clone(),
                progress: entry.progress,
                expert_name: entry.expert_name.clone(),
                elapsed_ms: elapsed,
            });
        }
        // Write-through (§5.2): terminal transitions persist state/result/
        // error/completed_at/progress + the expert_reports split. Terminal
        // writes get ONE immediate retry on failure (§5.2), then are given
        // up — history may lose a row, the review itself must never die.
        if let (Some(db), Some(entry)) = (&self.db, terminal_snapshot) {
            if let Err(e) = db.complete(&entry).await {
                tracing::error!("failed to persist terminal state for task {task_id}: {e:#}; retrying once");
                if let Err(e) = db.complete(&entry).await {
                    tracing::error!(
                        "terminal write for task {task_id} failed again: {e:#}; \
                         giving up — history will miss this row"
                    );
                }
            }
        }
    }

    /// Back-fill a task's `source_meta` from `candidate`, filling only fields
    /// that are currently unset (`None` or blank/whitespace). Values already
    /// present — e.g. parsed from the request URL at enqueue time — always
    /// win, so a late fill (MR metadata resolved mid-review) can never
    /// clobber enqueue-time metadata.
    ///
    /// No lifecycle event is broadcast: this is metadata enrichment, not a
    /// state transition. The next state-change event (`review.completed`, …)
    /// already reads the updated `source_meta`.
    ///
    /// Like [`update`](Self::update), a cancelled task is left untouched.
    pub async fn fill_source_meta(&self, task_id: Uuid, candidate: SourceMeta) {
        fn is_blank(value: &Option<String>) -> bool {
            value.as_deref().map(str::trim).unwrap_or_default().is_empty()
        }
        let mut filled = None;
        if let Some(entry) = self.inner.write().await.get_mut(&task_id) {
            if entry.state == TaskState::Cancelled {
                return;
            }
            let meta = &mut entry.source_meta;
            if is_blank(&meta.mr_title) {
                meta.mr_title = candidate.mr_title;
            }
            if is_blank(&meta.project) {
                meta.project = candidate.project;
            }
            if is_blank(&meta.repository) {
                meta.repository = candidate.repository;
            }
            if is_blank(&meta.branch) {
                meta.branch = candidate.branch;
            }
            if is_blank(&meta.target_branch) {
                meta.target_branch = candidate.target_branch;
            }
            if is_blank(&meta.author_name) {
                meta.author_name = candidate.author_name;
            }
            if is_blank(&meta.author_avatar_url) {
                meta.author_avatar_url = candidate.author_avatar_url;
            }
            if is_blank(&meta.gitlab_mr_url) {
                meta.gitlab_mr_url = candidate.gitlab_mr_url;
            }
            if is_blank(&meta.commit_sha) {
                meta.commit_sha = candidate.commit_sha;
            }
            filled = Some(entry.source_meta.clone());
        }
        // Write-through (§5.2): UPDATE source_meta + the materialized
        // project/repository filter columns. At most once per task.
        if let (Some(db), Some(meta)) = (&self.db, filled) {
            if let Err(e) = db.fill_source_meta(task_id, &meta).await {
                tracing::error!("failed to persist source_meta for task {task_id}: {e:#}");
            }
        }
    }

    pub async fn get(&self, task_id: Uuid) -> Option<TaskEntry> {
        self.inner.read().await.get(&task_id).cloned()
    }

    pub async fn list(
        &self,
        status: Option<TaskState>,
        page: u64,
        per_page: u64,
        q: Option<&str>,
        project: Option<&str>,
        repository: Option<&str>,
        date_from: Option<chrono::DateTime<chrono::Utc>>,
        date_to: Option<chrono::DateTime<chrono::Utc>>,
    ) -> (Vec<TaskEntry>, u64) {
        let map = self.inner.read().await;
        let mut filtered: Vec<TaskEntry> = map.values().cloned().collect();

        if let Some(s) = status {
            filtered.retain(|e| e.state == s);
        }

        if let Some(q_str) = q {
            let q_lower = q_str.to_lowercase();
            filtered.retain(|e| {
                let meta = &e.source_meta;
                meta.mr_title.as_deref().unwrap_or("").to_lowercase().contains(&q_lower)
                    || meta.project.as_deref().unwrap_or("").to_lowercase().contains(&q_lower)
                    || meta
                        .repository
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&q_lower)
                    || meta.branch.as_deref().unwrap_or("").to_lowercase().contains(&q_lower)
                    || meta
                        .author_name
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&q_lower)
                    || meta
                        .commit_sha
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&q_lower)
            });
        }

        if let Some(p) = project {
            filtered.retain(|e| e.source_meta.project.as_deref() == Some(p));
        }

        if let Some(r) = repository {
            filtered.retain(|e| e.source_meta.repository.as_deref() == Some(r));
        }

        if let Some(from) = date_from {
            filtered.retain(|e| e.created_at >= from);
        }

        if let Some(to) = date_to {
            filtered.retain(|e| e.created_at <= to);
        }

        let total = filtered.len() as u64;
        // Sort by created_at descending so pagination works correctly
        filtered.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        let skip = ((page.saturating_sub(1)) * per_page) as usize;
        let items: Vec<TaskEntry> = filtered.into_iter().skip(skip).take(per_page as usize).collect();
        (items, total)
    }

    /// Cancel a queued or running task by migrating it to [`TaskState::Cancelled`].
    ///
    /// The record is kept (not physically removed) so history pages can show it
    /// as `cancelled`. Tasks that are already in a terminal state (`Completed`,
    /// `Failed`, `Cancelled`) are left untouched and return `false`.
    pub async fn delete(&self, task_id: Uuid) -> bool {
        let mut cancelled_at = None;
        let transitioned = {
            let mut map = self.inner.write().await;
            if let Some(entry) = map.get_mut(&task_id) {
                if entry.state == TaskState::Pending || entry.state == TaskState::Running {
                    entry.state = TaskState::Cancelled;
                    let now = chrono::Utc::now();
                    entry.completed_at = Some(now);
                    cancelled_at = Some(now);
                    let meta = entry.source_meta.clone();
                    let _ = self.tx.send(TaskEvent {
                        task_id,
                        status: "cancelled",
                        event: "review.cancelled",
                        mr_title: meta.mr_title,
                        project: meta.project,
                        progress: None,
                        expert_name: None,
                        elapsed_ms: None,
                    });
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        // Write-through (§5.2): UPDATE state=cancelled, completed_at.
        if transitioned {
            if let (Some(db), Some(at)) = (&self.db, cancelled_at) {
                if let Err(e) = db.mark_cancelled(task_id, at).await {
                    tracing::error!("failed to persist cancellation for task {task_id}: {e:#}");
                }
            }
        }
        transitioned
    }

    pub async fn retry(&self, task_id: Uuid) -> bool {
        let transitioned = {
            let mut map = self.inner.write().await;
            if let Some(entry) = map.get_mut(&task_id) {
                if entry.state == TaskState::Failed {
                    entry.state = TaskState::Pending;
                    entry.error = None;
                    entry.progress = None;
                    entry.completed_at = None;
                    entry.started_at = None;
                    let meta = entry.source_meta.clone();
                    let _ = self.tx.send(TaskEvent {
                        task_id,
                        status: "pending",
                        event: "review.retry",
                        mr_title: meta.mr_title,
                        project: meta.project,
                        progress: None,
                        expert_name: None,
                        elapsed_ms: None,
                    });
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        // Write-through (§5.2): UPDATE state=pending, error=NULL,
        // completed_at=NULL (Failed → Pending).
        if transitioned {
            if let Some(db) = &self.db {
                if let Err(e) = db.mark_retry(task_id).await {
                    tracing::error!("failed to persist retry for task {task_id}: {e:#}");
                }
            }
        }
        transitioned
    }

    /// Aggregate queue statistics from the current task store.
    pub async fn queue_stats(&self) -> QueueStats {
        let map = self.inner.read().await;
        let mut active = 0u64;
        let mut queued = 0u64;
        let mut failed = 0u64;
        let mut failed_last_24h = 0u64;
        let mut total_last_24h = 0u64;
        let cutoff_24h = chrono::Utc::now() - chrono::Duration::hours(24);

        for entry in map.values() {
            match entry.state {
                TaskState::Running => active += 1,
                TaskState::Pending => queued += 1,
                TaskState::Failed => failed += 1,
                // Completed and Cancelled are terminal; Cancelled is not
                // counted as a failure in any queue stat.
                TaskState::Completed | TaskState::Cancelled => {}
            }
            if entry.created_at >= cutoff_24h {
                total_last_24h += 1;
                if entry.state == TaskState::Failed {
                    failed_last_24h += 1;
                }
            }
        }

        let max_concurrent = *self.max_concurrent.read().await as u64;
        let queue_capacity = *self.queue_capacity.read().await as u64;
        let is_paused = *self.is_paused.read().await;

        QueueStats {
            active,
            queued,
            failed,
            total_depth: active + queued,
            max_concurrent,
            queue_capacity,
            failed_last_24h,
            total_last_24h,
            is_paused,
        }
    }

    /// Pause the queue: new tasks will remain pending but will not be started.
    pub async fn pause(&self) {
        let mut paused = self.is_paused.write().await;
        *paused = true;
    }

    /// Resume the queue: allow new tasks to be started up to max_concurrent.
    pub async fn resume(&self) {
        let mut paused = self.is_paused.write().await;
        *paused = false;
    }

    /// Check whether the queue is currently paused.
    pub async fn is_paused(&self) -> bool {
        *self.is_paused.read().await
    }

    /// Set the maximum number of concurrently running tasks.
    pub async fn set_max_concurrent(&self, n: usize) {
        let mut mc = self.max_concurrent.write().await;
        *mc = n;
    }

    /// Get the current maximum number of concurrently running tasks.
    pub async fn get_max_concurrent(&self) -> usize {
        *self.max_concurrent.read().await
    }

    /// Set the queue capacity (max total depth).
    pub async fn set_queue_capacity(&self, n: usize) {
        let mut qc = self.queue_capacity.write().await;
        *qc = n;
    }

    /// Get the current queue capacity.
    pub async fn get_queue_capacity(&self) -> usize {
        *self.queue_capacity.read().await
    }

    /// Determine whether a new task may be started given pause and concurrency limits.
    pub async fn can_start_new_task(&self) -> bool {
        if *self.is_paused.read().await {
            return false;
        }
        let max = *self.max_concurrent.read().await;
        let active = self.active_count().await;
        active < max
    }

    /// Count currently running tasks.
    pub async fn active_count(&self) -> usize {
        let map = self.inner.read().await;
        map.values().filter(|e| e.state == TaskState::Running).count()
    }
}

/// Create a task-store entry from `source_meta` and mark it [`TaskState::Running`].
///
/// Webhook-dispatched reviews call this after the dispatcher has accepted the
/// URL+SHA pair, so each actually-started review records exactly one entry.
pub async fn record_task_started(store: &TaskStore, meta: SourceMeta) -> Uuid {
    let task_id = store.create(Some(meta)).await;
    store.start(task_id).await;
    task_id
}

/// Record the outcome of a webhook-dispatched review in the task store.
///
/// `Ok(output)` → [`TaskState::Completed`] with the full [`ReviewOutput`] JSON
/// as the result (so the History detail panel can render expert reports).
/// `Err` → [`TaskState::Failed`] with the error message.
pub async fn record_task_outcome(
    store: &TaskStore,
    task_id: Uuid,
    outcome: &anyhow::Result<crate::models::ReviewOutput>,
) {
    match outcome {
        Ok(output) => match serde_json::to_value(output) {
            Ok(result) => {
                store.update(task_id, TaskState::Completed, Some(result), None).await;
            }
            Err(e) => {
                let message = format!("failed to serialize ReviewOutput: {e:#}");
                tracing::warn!("{message}");
                store.update(task_id, TaskState::Failed, None, Some(message)).await;
            }
        },
        Err(e) => {
            store
                .update(task_id, TaskState::Failed, None, Some(format!("{e:#}")))
                .await;
        }
    }
}

impl TaskEntry {
    pub fn duration_ms(&self) -> Option<u64> {
        match (self.created_at, self.completed_at) {
            (start, Some(end)) => Some((end - start).num_milliseconds() as u64),
            _ => None,
        }
    }

    pub fn elapsed_ms(&self) -> Option<u64> {
        self.started_at
            .map(|s| (chrono::Utc::now() - s).num_milliseconds() as u64)
    }
}

/// Queue statistics returned by the queue monitor API.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueueStats {
    pub active: u64,
    pub queued: u64,
    pub failed: u64,
    pub total_depth: u64,
    pub max_concurrent: u64,
    pub queue_capacity: u64,
    pub failed_last_24h: u64,
    pub total_last_24h: u64,
    pub is_paused: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate_meta() -> SourceMeta {
        SourceMeta {
            mr_title: Some("Add login endpoint".to_string()),
            project: Some("group/proj".to_string()),
            repository: Some("group/proj".to_string()),
            branch: Some("feature/login".to_string()),
            target_branch: Some("main".to_string()),
            author_name: Some("alice".to_string()),
            author_avatar_url: None,
            gitlab_mr_url: None,
            commit_sha: Some("deadbeef".to_string()),
        }
    }

    #[tokio::test]
    async fn fill_source_meta_populates_blank_fields() {
        let store = TaskStore::new();
        let id = store.create(Some(SourceMeta::default())).await;

        store.fill_source_meta(id, candidate_meta()).await;

        let meta = store.get(id).await.expect("task exists").source_meta;
        assert_eq!(meta.mr_title.as_deref(), Some("Add login endpoint"));
        assert_eq!(meta.branch.as_deref(), Some("feature/login"));
        assert_eq!(meta.target_branch.as_deref(), Some("main"));
        assert_eq!(meta.author_name.as_deref(), Some("alice"));
        assert_eq!(meta.commit_sha.as_deref(), Some("deadbeef"));
        // Candidate-absent fields stay absent.
        assert!(meta.author_avatar_url.is_none());
        assert!(meta.gitlab_mr_url.is_none());
    }

    #[tokio::test]
    async fn fill_source_meta_never_clobbers_existing_values() {
        let store = TaskStore::new();
        let id = store
            .create(Some(SourceMeta {
                mr_title: Some("webhook title".to_string()),
                branch: Some("webhook-branch".to_string()),
                ..SourceMeta::default()
            }))
            .await;

        store.fill_source_meta(id, candidate_meta()).await;

        let meta = store.get(id).await.expect("task exists").source_meta;
        // Pre-existing (enqueue/webhook-provided) values win...
        assert_eq!(meta.mr_title.as_deref(), Some("webhook title"));
        assert_eq!(meta.branch.as_deref(), Some("webhook-branch"));
        // ...while still-blank fields are filled.
        assert_eq!(meta.target_branch.as_deref(), Some("main"));
        assert_eq!(meta.author_name.as_deref(), Some("alice"));
        assert_eq!(meta.commit_sha.as_deref(), Some("deadbeef"));
    }

    #[tokio::test]
    async fn fill_source_meta_treats_empty_and_whitespace_as_blank() {
        let store = TaskStore::new();
        let id = store
            .create(Some(SourceMeta {
                mr_title: Some(String::new()),
                branch: Some("   ".to_string()),
                ..SourceMeta::default()
            }))
            .await;

        store.fill_source_meta(id, candidate_meta()).await;

        let meta = store.get(id).await.expect("task exists").source_meta;
        assert_eq!(meta.mr_title.as_deref(), Some("Add login endpoint"));
        assert_eq!(meta.branch.as_deref(), Some("feature/login"));
    }

    #[tokio::test]
    async fn fill_source_meta_skips_cancelled_tasks() {
        let store = TaskStore::new();
        let id = store.create(Some(SourceMeta::default())).await;
        assert!(store.delete(id).await, "pending task must cancel");

        store.fill_source_meta(id, candidate_meta()).await;

        let meta = store.get(id).await.expect("cancelled record is kept").source_meta;
        assert!(
            meta.mr_title.is_none() && meta.branch.is_none() && meta.commit_sha.is_none(),
            "a cancelled task must not be mutated, got {meta:?}"
        );
    }

    // ─── 0.10.0 write-through (design/persistence.md §5.2) ───

    use crate::store::traits::ReviewStore;
    use crate::store::SqlxStore;

    async fn db_backed_store() -> (TaskStore, Arc<SqlxStore>) {
        let db = Arc::new(SqlxStore::new_in_memory().await.unwrap());
        db.migrate().await.unwrap();
        let mut store = TaskStore::new();
        store.set_db(db.clone());
        (store, db)
    }

    /// (state, project, repository, result, error, progress, started_at, completed_at)
    type ReviewRowTuple = (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
    );

    async fn review_row(db: &SqlxStore, task_id: Uuid) -> Option<ReviewRowTuple> {
        sqlx::query_as(
            "SELECT state, project, repository, result, error, progress, started_at, completed_at \
             FROM reviews WHERE task_id = ?",
        )
        .bind(task_id.to_string())
        .fetch_optional(db.pool())
        .await
        .unwrap()
    }

    fn sample_output() -> crate::models::ReviewOutput {
        let report = |name: &str| crate::models::ExpertReport {
            expert_name: name.to_string(),
            findings: vec![],
            markdown: format!("# {name} report"),
            raw_llm_response: "raw".to_string(),
            parse_error: None,
            raw_dump_path: None,
        };
        crate::models::ReviewOutput {
            reports: vec![report("security"), report("performance")],
            aggregated: None,
            dropped_findings: vec![],
            consolidated: None,
        }
    }

    /// (a) full lifecycle: create → start → fill_meta → complete, asserting
    /// the DB row at every step.
    #[tokio::test]
    async fn write_through_full_lifecycle() {
        let (store, db) = db_backed_store().await;
        let request = serde_json::json!({"mr_url": "https://gitlab.example/group/proj/-/merge_requests/1"});
        let id = store
            .create_with_request(
                Some(SourceMeta {
                    project: Some("group/proj".to_string()),
                    repository: Some("proj".to_string()),
                    ..SourceMeta::default()
                }),
                Some(request.clone()),
            )
            .await;

        // create → INSERT (pending), request + materialized columns persisted.
        let row = review_row(&db, id).await.expect("row exists after create");
        assert_eq!(row.0, "pending");
        assert_eq!(row.1.as_deref(), Some("group/proj"));
        assert_eq!(row.2.as_deref(), Some("proj"));
        assert!(row.3.is_none() && row.4.is_none());
        assert!(row.6.is_none() && row.7.is_none());
        let req: String = sqlx::query_scalar("SELECT request FROM reviews WHERE task_id = ?")
            .bind(id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&req).unwrap(), request);
        let created_at: String = sqlx::query_scalar("SELECT created_at FROM reviews WHERE task_id = ?")
            .bind(id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
        crate::store::decode_ts(&created_at).unwrap();

        // start → state=running + started_at.
        store.start(id).await;
        let row = review_row(&db, id).await.unwrap();
        assert_eq!(row.0, "running");
        assert!(row.6.is_some(), "started_at persisted");

        // fill_source_meta → source_meta updated; enqueue-time project wins.
        store.fill_source_meta(id, candidate_meta()).await;
        let meta_raw: String = sqlx::query_scalar("SELECT source_meta FROM reviews WHERE task_id = ?")
            .bind(id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
        let meta: SourceMeta = serde_json::from_str(&meta_raw).unwrap();
        assert_eq!(meta.mr_title.as_deref(), Some("Add login endpoint"));
        assert_eq!(meta.branch.as_deref(), Some("feature/login"));
        assert_eq!(
            meta.project.as_deref(),
            Some("group/proj"),
            "enqueue-time value must win"
        );
        let row = review_row(&db, id).await.unwrap();
        assert_eq!(row.1.as_deref(), Some("group/proj"), "materialized column in sync");

        // set_progress must NOT write: progress stays NULL mid-flight.
        store.set_progress(id, 42, Some("security".to_string())).await;
        let row = review_row(&db, id).await.unwrap();
        assert!(row.5.is_none(), "progress is not persisted mid-flight");

        // complete → terminal row: state/result/completed_at + progress snapshot.
        let result = serde_json::to_value(sample_output()).unwrap();
        store.update(id, TaskState::Completed, Some(result.clone()), None).await;
        let row = review_row(&db, id).await.unwrap();
        assert_eq!(row.0, "completed");
        assert!(row.7.is_some(), "completed_at persisted");
        assert_eq!(row.5, Some(42), "terminal write snapshots progress");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&row.3.unwrap()).unwrap(),
            result
        );
    }

    /// (b) complete splits result.reports into one expert_reports row each.
    #[tokio::test]
    async fn write_through_complete_splits_expert_reports() {
        let (store, db) = db_backed_store().await;
        let id = record_task_started(&store, SourceMeta::default()).await;
        let output = sample_output();
        store
            .update(
                id,
                TaskState::Completed,
                Some(serde_json::to_value(&output).unwrap()),
                None,
            )
            .await;

        let reports: Vec<(String, String, Option<i64>, String)> = sqlx::query_as(
            "SELECT expert_name, report, duration_ms, created_at FROM expert_reports \
             WHERE task_id = ? ORDER BY expert_name",
        )
        .bind(id.to_string())
        .fetch_all(db.pool())
        .await
        .unwrap();
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].0, "performance");
        assert_eq!(reports[1].0, "security");
        for (_, report_json, duration_ms, created_at) in &reports {
            let decoded: crate::models::ExpertReport = serde_json::from_str(report_json).unwrap();
            assert!(decoded.markdown.starts_with("# "));
            assert!(duration_ms.is_none(), "per-expert duration is NULL for now (§5.4)");
            crate::store::decode_ts(created_at).unwrap();
        }
        assert_eq!(reports[1].0, output.reports[0].expert_name);

        // A failed terminal write stores error, no result, no reports.
        let id2 = record_task_started(&store, SourceMeta::default()).await;
        store
            .update(id2, TaskState::Failed, None, Some("boom".to_string()))
            .await;
        let row = review_row(&db, id2).await.unwrap();
        assert_eq!(row.0, "failed");
        assert_eq!(row.4.as_deref(), Some("boom"));
        assert!(row.3.is_none());
        let report_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM expert_reports WHERE task_id = ?")
            .bind(id2.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(report_count, 0);
    }

    /// delete (cancel) and retry write-through.
    #[tokio::test]
    async fn write_through_cancel_and_retry() {
        let (store, db) = db_backed_store().await;

        let id = store.create(None).await;
        assert!(store.delete(id).await);
        let row = review_row(&db, id).await.unwrap();
        assert_eq!(row.0, "cancelled");
        assert!(row.7.is_some(), "cancel stamps completed_at");

        // Cancelling a terminal task touches nothing, in memory or in DB.
        assert!(!store.delete(id).await);

        let id2 = record_task_started(&store, SourceMeta::default()).await;
        store
            .update(id2, TaskState::Failed, None, Some("boom".to_string()))
            .await;
        assert!(store.retry(id2).await);
        let row = review_row(&db, id2).await.unwrap();
        assert_eq!(row.0, "pending");
        assert!(row.4.is_none(), "retry clears error");
        assert!(row.7.is_none(), "retry clears completed_at");
    }

    /// (c) a failing ReviewStore never blocks the review path; the terminal
    /// write is retried exactly once and then given up.
    #[tokio::test]
    async fn write_through_failure_never_blocks_review_path() {
        #[derive(Default)]
        struct FailingStore {
            complete_attempts: std::sync::atomic::AtomicUsize,
        }

        #[async_trait::async_trait]
        impl ReviewStore for FailingStore {
            async fn create(&self, _: &TaskEntry) -> anyhow::Result<()> {
                anyhow::bail!("db down")
            }
            async fn mark_started(&self, _: Uuid, _: chrono::DateTime<chrono::Utc>) -> anyhow::Result<()> {
                anyhow::bail!("db down")
            }
            async fn fill_source_meta(&self, _: Uuid, _: &SourceMeta) -> anyhow::Result<()> {
                anyhow::bail!("db down")
            }
            async fn complete(&self, _: &TaskEntry) -> anyhow::Result<()> {
                self.complete_attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                anyhow::bail!("db down")
            }
            async fn mark_cancelled(&self, _: Uuid, _: chrono::DateTime<chrono::Utc>) -> anyhow::Result<()> {
                anyhow::bail!("db down")
            }
            async fn mark_retry(&self, _: Uuid) -> anyhow::Result<()> {
                anyhow::bail!("db down")
            }
            async fn mark_interrupted(&self, _: chrono::DateTime<chrono::Utc>) -> anyhow::Result<u64> {
                anyhow::bail!("db down")
            }
            async fn list_reviews(
                &self,
                _: &crate::store::traits::ReviewListQuery,
            ) -> anyhow::Result<(Vec<TaskEntry>, u64)> {
                anyhow::bail!("db down")
            }
            async fn get_review(&self, _: Uuid) -> anyhow::Result<Option<TaskEntry>> {
                anyhow::bail!("db down")
            }
            async fn upsert_review_context(&self, _: Uuid, _: &str, _: &str, _: &str, _: i64) -> anyhow::Result<()> {
                anyhow::bail!("db down")
            }
        }

        let failing = Arc::new(FailingStore::default());
        let mut store = TaskStore::new();
        store.set_db(failing.clone());

        let id = record_task_started(&store, SourceMeta::default()).await;
        store
            .update(
                id,
                TaskState::Completed,
                Some(serde_json::to_value(sample_output()).unwrap()),
                None,
            )
            .await;

        // The review path is unaffected: the in-memory entry is Completed.
        let entry = store.get(id).await.expect("entry exists");
        assert_eq!(entry.state, TaskState::Completed);
        assert!(entry.result.is_some());
        assert_eq!(
            failing.complete_attempts.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "terminal write is retried exactly once, then given up"
        );
    }

    /// (e) with no DB injected the store is exactly the 0.9 in-memory store.
    #[tokio::test]
    async fn no_db_behaves_like_0_9() {
        let store = TaskStore::new();
        let id = record_task_started(&store, SourceMeta::default()).await;
        store
            .update(id, TaskState::Completed, Some(serde_json::json!({"ok": true})), None)
            .await;
        let entry = store.get(id).await.unwrap();
        assert_eq!(entry.state, TaskState::Completed);
        assert!(!store.retry(id).await, "retry only from Failed");
        store.update(id, TaskState::Failed, None, Some("x".to_string())).await;
        assert!(store.retry(id).await);
        assert_eq!(store.get(id).await.unwrap().state, TaskState::Pending);
    }
}
