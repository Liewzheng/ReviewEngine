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

#[derive(Debug, Clone)]
pub struct TaskEntry {
    pub task_id: Uuid,
    pub state: TaskState,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TaskEvent {
    pub task_id: Uuid,
    pub status: &'static str,
    pub event: &'static str,
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

    pub async fn create(&self) -> Uuid {
        let id = Uuid::new_v4();
        let entry = TaskEntry {
            task_id: id,
            state: TaskState::Pending,
            created_at: chrono::Utc::now(),
            completed_at: None,
            result: None,
            error: None,
        };
        self.inner.write().await.insert(id, entry);
        let _ = self.tx.send(TaskEvent {
            task_id: id,
            status: "pending",
            event: "review.created",
        });
        id
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
            entry.error = error;
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
            let _ = self.tx.send(TaskEvent { task_id, status, event });
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
                map.remove(&task_id);
                let _ = self.tx.send(TaskEvent {
                    task_id,
                    status: "cancelled",
                    event: "review.cancelled",
                });
                return true;
            }
        }
        false
    }
}

impl TaskEntry {
    pub fn duration_ms(&self) -> Option<u64> {
        match (self.created_at, self.completed_at) {
            (start, Some(end)) => Some((end - start).num_milliseconds() as u64),
            _ => None,
        }
    }
}
