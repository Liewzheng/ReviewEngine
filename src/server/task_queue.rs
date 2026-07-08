use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub enum TaskState {
    Pending,
    Running,
    Completed,
    Failed,
}

/// Metadata about the source merge request or pull request.
#[derive(Debug, Clone, Default)]
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

#[derive(Debug, Clone)]
pub struct TaskEntry {
    pub task_id: Uuid,
    pub state: TaskState,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    pub source_meta: SourceMeta,
    pub progress: Option<u8>,        // 0-100
    pub expert_name: Option<String>, // current active expert
}

#[derive(Debug, Clone)]
pub struct TaskEvent {
    pub task_id: Uuid,
    pub status: &'static str,
    pub event: &'static str,
    pub mr_title: Option<String>,
    pub project: Option<String>,
    pub progress: Option<u8>,
    pub expert_name: Option<String>,
    pub elapsed_ms: Option<u64>,
}

#[derive(Clone)]
pub struct TaskStore {
    inner: Arc<RwLock<HashMap<Uuid, TaskEntry>>>,
    tx: tokio::sync::broadcast::Sender<TaskEvent>,
    is_paused: Arc<RwLock<bool>>,
    max_concurrent: Arc<RwLock<usize>>,
    queue_capacity: Arc<RwLock<usize>>,
}

impl TaskStore {
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(256);
        let inner: Arc<RwLock<HashMap<Uuid, TaskEntry>>> = Arc::new(RwLock::new(HashMap::new()));
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

        Self {
            inner,
            tx,
            is_paused: Arc::new(RwLock::new(false)),
            max_concurrent: Arc::new(RwLock::new(8)),
            queue_capacity: Arc::new(RwLock::new(16)),
        }
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
        let id = Uuid::new_v4();
        let entry = TaskEntry {
            task_id: id,
            state: TaskState::Pending,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            result: None,
            error: None,
            source_meta: source_meta.unwrap_or_default(),
            progress: None,
            expert_name: None,
        };
        self.inner.write().await.insert(id, entry);
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
        id
    }

    pub async fn start(&self, task_id: Uuid) {
        if let Some(entry) = self.inner.write().await.get_mut(&task_id) {
            entry.state = TaskState::Running;
            entry.started_at = Some(chrono::Utc::now());
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
        if let Some(entry) = self.inner.write().await.get_mut(&task_id) {
            entry.state = new_state.clone();
            entry.result = result;
            entry.error = error.clone();
            if new_state == TaskState::Completed || new_state == TaskState::Failed {
                entry.completed_at = Some(chrono::Utc::now());
            }
            let event = match new_state {
                TaskState::Pending => "review.created",
                TaskState::Running => "review.started",
                TaskState::Completed => "review.completed",
                TaskState::Failed => "review.failed",
            };
            let status = match new_state {
                TaskState::Pending => "pending",
                TaskState::Running => "running",
                TaskState::Completed => "completed",
                TaskState::Failed => "failed",
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

    pub async fn delete(&self, task_id: Uuid) -> bool {
        let mut map = self.inner.write().await;
        if let Some(entry) = map.get(&task_id) {
            if entry.state == TaskState::Pending || entry.state == TaskState::Running {
                let meta = entry.source_meta.clone();
                map.remove(&task_id);
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
                return true;
            }
        }
        false
    }

    pub async fn retry(&self, task_id: Uuid) -> bool {
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
                return true;
            }
        }
        false
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
                _ => {}
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
