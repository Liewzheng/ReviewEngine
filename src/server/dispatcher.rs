//! MR webhook dispatch deduplication. Prevents concurrent reviews of the same MR.
//!
//! @module review-engine: CodeReview Board platform
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{watch, Mutex};

/// MR 分发去重器。跨 webhook 共享的单例。
///
/// 对每个 MR 跟踪 review 状态（是否正在运行、最后一次审核的 commit SHA），
/// 避免同一 MR 的并发 push 事件触发多次审核。
#[derive(Clone)]
pub struct MrDispatcher {
    inner: Arc<Mutex<HashMap<String, Arc<Mutex<MrStatus>>>>>,
}

struct MrStatus {
    running: bool,
    last_sha: Option<String>,
    signal_tx: watch::Sender<bool>,
    signal_rx: watch::Receiver<bool>,
}

/// `try_start` 的返回结果。
#[derive(Debug, Clone, PartialEq)]
pub enum ShouldStart {
    /// 新工作，可以启动 review。
    Go,
    /// 此 SHA 已审核过，跳过。
    AlreadyReviewed,
    /// 当前有 review 正在运行，调用方应等待。
    InProgress,
}

impl MrDispatcher {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 尝试启动 review。
    ///
    /// 两层 Mutex：外层保护 HashMap 的插入/删除，
    /// 内层保护单个 MR 的状态读写，避免粗粒度锁阻塞不相干的 MR。
    pub async fn try_start(&self, mr_url: &str, sha: &str) -> ShouldStart {
        let mut map = self.inner.lock().await;
        let entry = map.entry(mr_url.to_string()).or_insert_with(|| {
            let (signal_tx, signal_rx) = watch::channel(false);
            Arc::new(Mutex::new(MrStatus {
                running: false,
                last_sha: None,
                signal_tx,
                signal_rx,
            }))
        });
        let mut status = entry.lock().await;

        if status.running {
            return ShouldStart::InProgress;
        }

        if status.last_sha.as_deref() == Some(sha) {
            return ShouldStart::AlreadyReviewed;
        }

        status.running = true;
        ShouldStart::Go
    }

    /// 标记 review 完成，记录 SHA，通知等待者。
    pub async fn complete(&self, mr_url: &str, sha: &str) {
        let map = self.inner.lock().await;
        if let Some(entry) = map.get(mr_url) {
            let mut status = entry.lock().await;
            status.running = false;
            status.last_sha = Some(sha.to_string());
            status.signal_tx.send(true).ok();
        }
    }

    /// 等待当前 review 完成。
    pub async fn wait(&self, mr_url: &str) {
        let mut rx = {
            let map = self.inner.lock().await;
            if let Some(entry) = map.get(mr_url) {
                let status = entry.lock().await;
                if status.running {
                    Some(status.signal_rx.clone())
                } else {
                    None
                }
            } else {
                None
            }
        };

        if let Some(ref mut rx) = rx {
            if *rx.borrow() {
                return;
            }
            let _ = rx.changed().await;
        }
    }

    /// 移除 MR 条目（合并/关闭后调用）。
    pub async fn remove(&self, mr_url: &str) {
        let mut map = self.inner.lock().await;
        map.remove(mr_url);
    }

    /// 重置 running 状态但不记录 SHA（用于 task panic 恢复）。
    pub async fn reset(&self, mr_url: &str) {
        let map = self.inner.lock().await;
        if let Some(entry) = map.get(mr_url) {
            let mut status = entry.lock().await;
            status.running = false;
            status.signal_tx.send(true).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_try_start_new_mr_returns_go() {
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
    }

    #[tokio::test]
    async fn test_try_start_same_sha_returns_already_reviewed() {
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
        d.complete("mr1", "sha1").await;
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::AlreadyReviewed);
    }

    #[tokio::test]
    async fn test_try_start_while_running_returns_in_progress() {
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
        // still running, not completed yet
        assert_eq!(d.try_start("mr1", "sha2").await, ShouldStart::InProgress);
    }

    #[tokio::test]
    async fn test_complete_and_new_sha_allows_go() {
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
        d.complete("mr1", "sha1").await;
        // new SHA after completion → Go
        assert_eq!(d.try_start("mr1", "sha2").await, ShouldStart::Go);
    }

    #[tokio::test]
    async fn test_wait_returns_immediately_when_not_running() {
        let d = MrDispatcher::new();
        // Not started yet — wait returns immediately
        d.wait("mr1").await;
    }

    #[tokio::test]
    async fn test_wait_returns_after_complete() {
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);

        let d2 = d.clone();
        let handle = tokio::spawn(async move {
            d2.complete("mr1", "sha1").await;
        });

        // Wait should return after complete is called
        d.wait("mr1").await;
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_wait_handles_early_completion() {
        // Edge case: complete() fires before wait() starts listening
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);

        // Signal before wait enters the await
        d.complete("mr1", "sha1").await;

        // wait() should see the signal was already sent and return immediately
        d.wait("mr1").await;
    }

    #[tokio::test]
    async fn test_remove_clears_entry() {
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
        d.complete("mr1", "sha1").await;
        d.remove("mr1").await;

        // After remove, MR is unknown again → Go (not AlreadyReviewed)
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
    }

    #[tokio::test]
    async fn test_reset_clears_running_flag() {
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);

        // Simulate panic recovery
        d.reset("mr1").await;

        // After reset, new SHA should get Go
        assert_eq!(d.try_start("mr1", "sha2").await, ShouldStart::Go);
    }

    #[tokio::test]
    async fn test_reset_does_not_record_sha() {
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
        d.reset("mr1").await;

        // Same SHA should NOT be AlreadyReviewed because reset doesn't record SHA
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
    }

    #[tokio::test]
    async fn test_different_mrs_do_not_interfere() {
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
        assert_eq!(d.try_start("mr2", "sha1").await, ShouldStart::Go);

        d.complete("mr1", "sha1").await;

        // mr2 should still be running
        assert_eq!(d.try_start("mr2", "sha2").await, ShouldStart::InProgress);
        // mr1 should accept new SHA
        assert_eq!(d.try_start("mr1", "sha2").await, ShouldStart::Go);
    }

    #[tokio::test]
    async fn test_concurrent_try_start_only_one_gets_go() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let d = Arc::new(MrDispatcher::new());
        let go_count = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..10 {
            let d = d.clone();
            let count = go_count.clone();
            handles.push(tokio::spawn(async move {
                if d.try_start("mr1", "sha1").await == ShouldStart::Go {
                    count.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(go_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_remove_during_running_does_not_panic() {
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
        // remove while running (should not panic)
        d.remove("mr1").await;
    }

    #[tokio::test]
    async fn test_complete_nonexistent_mr_does_not_panic() {
        let d = MrDispatcher::new();
        d.complete("nonexistent", "sha1").await;
    }

    #[tokio::test]
    async fn test_reset_nonexistent_mr_does_not_panic() {
        let d = MrDispatcher::new();
        d.reset("nonexistent").await;
    }

    #[tokio::test]
    async fn test_remove_nonexistent_mr_does_not_panic() {
        let d = MrDispatcher::new();
        d.remove("nonexistent").await;
    }
}
