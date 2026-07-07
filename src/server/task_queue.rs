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

        Self { inner, tx }
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

    pub async fn list(&self, status: Option<TaskState>, page: u64, per_page: u64) -> (Vec<TaskEntry>, u64) {
        let map = self.inner.read().await;
        let mut filtered: Vec<TaskEntry> = match status {
            Some(ref s) => map.values().filter(|e| e.state == *s).cloned().collect(),
            None => map.values().cloned().collect(),
        };
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

        QueueStats {
            active,
            queued,
            failed,
            total_depth: active + queued,
            max_concurrent: 8,  // TODO: derive from config
            queue_capacity: 16, // TODO: derive from config
            failed_last_24h,
            total_last_24h,
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
}
