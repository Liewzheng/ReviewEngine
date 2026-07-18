//! MR webhook dispatch deduplication. Prevents concurrent reviews of the same MR.
//!
//! @module review-engine: CodeReview Board platform
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{watch, Mutex};

/// Default age after which a `running` marker is considered stale — e.g. the
/// review task panicked or the process restarted mid-review.
const DEFAULT_TIMEOUT_SECS: u64 = 15 * 60;

/// Env var overriding [`DEFAULT_TIMEOUT_SECS`].
const TIMEOUT_ENV: &str = "REVIEW_DISPATCH_TIMEOUT_SECS";

/// Env var pointing at the JSON file used to persist dispatcher state.
const STATE_PATH_ENV: &str = "REVIEW_DISPATCH_STATE";

/// MR 分发去重器。跨 webhook 共享的单例。
///
/// 对每个 MR 跟踪 review 状态（running 起始时间、最后一次审核的 commit SHA），
/// 避免同一 MR 的并发 push 事件触发多次审核。
///
/// `running` 标记带有时间戳：超过超时（默认 15 分钟，可用
/// `REVIEW_DISPATCH_TIMEOUT_SECS` 调整）仍未 `complete` 的标记视为过期，
/// 允许重新发起 review —— 避免 panic 后 MR 永远卡在 running。
///
/// [`MrDispatcher::persistent`] 额外把状态落到 JSON 文件（默认
/// `~/.config/review-engine/dispatcher-state.json`，可用 `REVIEW_DISPATCH_STATE`
/// 覆盖），进程重启后不再丢失已审核的 SHA。
#[derive(Clone)]
pub struct MrDispatcher {
    inner: Arc<Mutex<HashMap<String, MrStatus>>>,
    state_path: Option<PathBuf>,
    timeout: Duration,
}

struct MrStatus {
    /// When the current review started; `None` when idle.
    running_since: Option<DateTime<Utc>>,
    last_sha: Option<String>,
    signal_tx: watch::Sender<bool>,
    signal_rx: watch::Receiver<bool>,
}

impl MrStatus {
    fn new(running_since: Option<DateTime<Utc>>, last_sha: Option<String>) -> Self {
        let (signal_tx, signal_rx) = watch::channel(false);
        Self {
            running_since,
            last_sha,
            signal_tx,
            signal_rx,
        }
    }
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

/// On-disk representation of the dispatcher state.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedState {
    entries: HashMap<String, PersistedEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedEntry {
    #[serde(default)]
    last_sha: Option<String>,
    #[serde(default)]
    running_since: Option<DateTime<Utc>>,
}

impl MrDispatcher {
    /// In-memory dispatcher with the default 15-minute timeout and no
    /// persistence (used by tests and one-shot webhook handlers).
    pub fn new() -> Self {
        Self::with_state_file(None, Duration::from_secs(DEFAULT_TIMEOUT_SECS))
    }

    /// Dispatcher used by the long-running server: persists state to
    /// `REVIEW_DISPATCH_STATE` (default `~/.config/review-engine/dispatcher-state.json`)
    /// and honours `REVIEW_DISPATCH_TIMEOUT_SECS`.
    pub fn persistent() -> Self {
        Self::with_state_file(default_state_path(), configured_timeout())
    }

    /// Explicit constructor: `state_path == None` disables persistence.
    /// Existing state is loaded from disk; expired `running` markers are
    /// cleared on load so a review interrupted by a restart can re-trigger.
    pub fn with_state_file(state_path: Option<PathBuf>, timeout: Duration) -> Self {
        let entries = state_path
            .as_deref()
            .map(|p| load_state(p, timeout))
            .unwrap_or_default();
        Self {
            inner: Arc::new(Mutex::new(entries)),
            state_path,
            timeout,
        }
    }

    /// 尝试启动 review。
    ///
    /// 外层 Mutex 保护整张状态表；所有临界区都极短（`wait` 在锁外等待），
    /// 同时保证持久化时能对全表做一致快照。
    pub async fn try_start(&self, mr_url: &str, sha: &str) -> ShouldStart {
        let mut map = self.inner.lock().await;
        let status = map
            .entry(mr_url.to_string())
            .or_insert_with(|| MrStatus::new(None, None));

        if let Some(since) = status.running_since {
            if is_expired(since, self.timeout) {
                tracing::warn!("Dispatcher: stale running marker for {mr_url} expired; allowing a new review");
                status.running_since = None;
            } else {
                return ShouldStart::InProgress;
            }
        }

        if status.last_sha.as_deref() == Some(sha) {
            return ShouldStart::AlreadyReviewed;
        }

        status.running_since = Some(Utc::now());
        self.persist_locked(&map);
        ShouldStart::Go
    }

    /// 标记 review 完成，记录 SHA，通知等待者。
    pub async fn complete(&self, mr_url: &str, sha: &str) {
        let mut map = self.inner.lock().await;
        if let Some(status) = map.get_mut(mr_url) {
            status.running_since = None;
            status.last_sha = Some(sha.to_string());
            status.signal_tx.send(true).ok();
            self.persist_locked(&map);
        }
    }

    /// 等待当前 review 完成。
    pub async fn wait(&self, mr_url: &str) {
        let mut rx = {
            let map = self.inner.lock().await;
            match map.get(mr_url) {
                Some(status) if status.running_since.is_some() => Some(status.signal_rx.clone()),
                _ => None,
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
        if map.remove(mr_url).is_some() {
            self.persist_locked(&map);
        }
    }

    /// 重置 running 状态但不记录 SHA（用于 task panic 恢复）。
    pub async fn reset(&self, mr_url: &str) {
        let mut map = self.inner.lock().await;
        if let Some(status) = map.get_mut(mr_url) {
            status.running_since = None;
            status.signal_tx.send(true).ok();
            self.persist_locked(&map);
        }
    }

    /// Write the current state to disk atomically (temp file + rename).
    /// Best-effort: failures are logged, never propagated to the caller.
    fn persist_locked(&self, map: &HashMap<String, MrStatus>) {
        let Some(path) = &self.state_path else { return };
        let state = PersistedState {
            entries: map
                .iter()
                .map(|(url, status)| {
                    (
                        url.clone(),
                        PersistedEntry {
                            last_sha: status.last_sha.clone(),
                            running_since: status.running_since,
                        },
                    )
                })
                .collect(),
        };
        if let Err(e) = write_state_atomic(path, &state) {
            tracing::warn!("Dispatcher: failed to persist state to {}: {e}", path.display());
        }
    }
}

/// A `running` marker is expired once it is older than `timeout`.
fn is_expired(since: DateTime<Utc>, timeout: Duration) -> bool {
    let secs = timeout.as_secs().min(i64::MAX as u64) as i64;
    Utc::now().signed_duration_since(since) >= chrono::Duration::seconds(secs)
}

/// Dispatch timeout: `REVIEW_DISPATCH_TIMEOUT_SECS` or the 15-minute default.
fn configured_timeout() -> Duration {
    std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map_or(Duration::from_secs(DEFAULT_TIMEOUT_SECS), Duration::from_secs)
}

/// State file location: `REVIEW_DISPATCH_STATE` or the default config path.
fn default_state_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var(STATE_PATH_ENV) {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    home::home_dir().map(|dir| dir.join(".config").join("review-engine").join("dispatcher-state.json"))
}

/// Load persisted state from disk, clearing expired `running` markers.
/// Missing or corrupt files yield an empty state (with a warn log).
fn load_state(path: &Path, timeout: Duration) -> HashMap<String, MrStatus> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
        Err(e) => {
            tracing::warn!("Dispatcher: failed to read state file {}: {e}", path.display());
            return HashMap::new();
        }
    };
    let state: PersistedState = match serde_json::from_str(&content) {
        Ok(state) => state,
        Err(e) => {
            tracing::warn!("Dispatcher: ignoring corrupt state file {}: {e}", path.display());
            return HashMap::new();
        }
    };
    state
        .entries
        .into_iter()
        .map(|(url, entry)| {
            let running_since = entry.running_since.and_then(|since| {
                if is_expired(since, timeout) {
                    tracing::warn!("Dispatcher: stale running marker for {url} expired on load");
                    None
                } else {
                    Some(since)
                }
            });
            (url, MrStatus::new(running_since, entry.last_sha))
        })
        .collect()
}

/// Serialize `state` to `path` atomically via a temp file + rename, so a
/// crash mid-write never leaves a truncated state file behind.
fn write_state_atomic(path: &Path, state: &PersistedState) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, json)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
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

    // ─── A10: timeout recovery & persistence ────────────────────────

    #[tokio::test]
    async fn test_expired_running_marker_allows_restart() {
        // Zero timeout: every running marker is immediately stale.
        let d = MrDispatcher::with_state_file(None, Duration::ZERO);
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
        // Marker aged past the timeout without complete() → Go again, not InProgress.
        assert_eq!(d.try_start("mr1", "sha2").await, ShouldStart::Go);
    }

    #[tokio::test]
    async fn test_fresh_running_marker_still_blocks() {
        let d = MrDispatcher::with_state_file(None, Duration::from_secs(3600));
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
        assert_eq!(d.try_start("mr1", "sha2").await, ShouldStart::InProgress);
    }

    #[tokio::test]
    async fn test_state_is_persisted_and_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join("dispatcher-state.json");

        let d1 = MrDispatcher::with_state_file(Some(path.clone()), Duration::from_secs(3600));
        assert_eq!(d1.try_start("mr1", "sha1").await, ShouldStart::Go);
        d1.complete("mr1", "sha1").await;
        drop(d1);

        // State file exists and records the completed SHA with no running marker.
        let content = std::fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(json["entries"]["mr1"]["last_sha"], "sha1");
        assert!(json["entries"]["mr1"]["running_since"].is_null());

        // A fresh dispatcher (e.g. after a process restart) remembers the SHA.
        let d2 = MrDispatcher::with_state_file(Some(path.clone()), Duration::from_secs(3600));
        assert_eq!(d2.try_start("mr1", "sha1").await, ShouldStart::AlreadyReviewed);
        assert_eq!(d2.try_start("mr1", "sha2").await, ShouldStart::Go);
    }

    #[tokio::test]
    async fn test_reload_expires_stale_running_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dispatcher-state.json");

        // Simulate a crash: running marker persisted, complete() never called.
        let d1 = MrDispatcher::with_state_file(Some(path.clone()), Duration::from_secs(3600));
        assert_eq!(d1.try_start("mr1", "sha1").await, ShouldStart::Go);
        drop(d1);

        // After restart with a zero timeout the stale marker is expired on load.
        let d2 = MrDispatcher::with_state_file(Some(path.clone()), Duration::ZERO);
        assert_eq!(d2.try_start("mr1", "sha1").await, ShouldStart::Go);
    }

    #[tokio::test]
    async fn test_reload_keeps_fresh_running_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dispatcher-state.json");

        let d1 = MrDispatcher::with_state_file(Some(path.clone()), Duration::from_secs(3600));
        assert_eq!(d1.try_start("mr1", "sha1").await, ShouldStart::Go);
        drop(d1);

        // Fresh marker survives a reload: still InProgress, no duplicate review.
        let d2 = MrDispatcher::with_state_file(Some(path.clone()), Duration::from_secs(3600));
        assert_eq!(d2.try_start("mr1", "sha2").await, ShouldStart::InProgress);
    }

    #[tokio::test]
    async fn test_corrupt_state_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dispatcher-state.json");
        std::fs::write(&path, "{ not json").unwrap();

        let d = MrDispatcher::with_state_file(Some(path), Duration::from_secs(3600));
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
    }

    #[tokio::test]
    async fn test_remove_persists_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dispatcher-state.json");

        let d1 = MrDispatcher::with_state_file(Some(path.clone()), Duration::from_secs(3600));
        assert_eq!(d1.try_start("mr1", "sha1").await, ShouldStart::Go);
        d1.complete("mr1", "sha1").await;
        d1.remove("mr1").await;
        drop(d1);

        // The removal is on disk: a fresh dispatcher treats the MR as unknown.
        let d2 = MrDispatcher::with_state_file(Some(path), Duration::from_secs(3600));
        assert_eq!(d2.try_start("mr1", "sha1").await, ShouldStart::Go);
    }

    #[test]
    fn test_write_state_atomic_leaves_no_tmp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dispatcher-state.json");
        let mut state = PersistedState::default();
        state.entries.insert(
            "mr1".to_string(),
            PersistedEntry {
                last_sha: Some("sha1".to_string()),
                running_since: None,
            },
        );
        write_state_atomic(&path, &state).unwrap();
        assert!(path.exists());
        assert!(!path.with_extension("tmp").exists());
    }
}
