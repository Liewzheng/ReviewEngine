//! MR webhook dispatch deduplication. Prevents concurrent reviews of the same MR.
//!
//! @module review-engine: CodeReview Board platform
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{watch, Mutex};

/// Default age after which a `running` marker is considered stale — e.g. the
/// review task panicked or the process restarted mid-review.
const DEFAULT_TIMEOUT_SECS: u64 = 15 * 60;

/// Env var overriding [`DEFAULT_TIMEOUT_SECS`].
const TIMEOUT_ENV: &str = "REVIEW_DISPATCH_TIMEOUT_SECS";

/// Env var pointing at the JSON file used to persist dispatcher state.
const STATE_PATH_ENV: &str = crate::paths::DISPATCH_STATE_ENV;

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
/// `~/.config/review-engine/dispatcher-state.json`，`serve --data-dir` 会把它移到
/// 该目录下，也可用 `REVIEW_DISPATCH_STATE` 直接覆盖），进程重启后不再丢失已审核的 SHA。
///
/// 去重同时看 **SHA** 与 **内容指纹**（[`content_fingerprint`]）：SHA 相同必然
/// 跳过；SHA 变了但 diff 逐字节相同（amend / force-push 到同样内容）也跳过，
/// 于是"同一处小改动反复推送"不再每轮烧一次 LLM。
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
    /// [`content_fingerprint`] of the diff the last review ran on.
    last_fingerprint: Option<String>,
    signal_tx: watch::Sender<bool>,
    signal_rx: watch::Receiver<bool>,
}

impl MrStatus {
    fn new(running_since: Option<DateTime<Utc>>, last_sha: Option<String>, last_fingerprint: Option<String>) -> Self {
        let (signal_tx, signal_rx) = watch::channel(false);
        Self {
            running_since,
            last_sha,
            last_fingerprint,
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

/// [`MrDispatcher::wait`] 的返回结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// Review 已完成，或本来就没有正在运行的 review 需要等待（含条目被移除）。
    Completed,
    /// 在 running-marker 超时周期内未观察到完成信号。调用方不应永久挂起，
    /// 应重新调用 [`MrDispatcher::try_start`]：此时过期标记通常已被清除，
    /// 会返回 `Go` 从而走 abort 恢复路径。
    TimedOut,
}

/// Whether a dispatch may be skipped because the content is unchanged (RENG-62).
///
/// The gate answers a question the SHA alone cannot: an amend / force-push
/// carries a **new** SHA over **identical** content, so `try_start`'s SHA check
/// passes while the review would reproduce exactly the round already posted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentGate {
    /// Apply the gate — webhook push/update events, where a redundant round
    /// costs LLM spend and re-posts the same comments.
    Enabled,
    /// Never skip. An explicit, user-triggered run (`/review` comment, REST
    /// submit/rerun) must always produce a fresh review even when the content
    /// is byte-identical to the last round.
    Bypassed,
}

/// [`MrDispatcher::claim_content`] 的返回结果，附带可直接写日志的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentDecision {
    /// 需要审核：首次见到该 MR，或内容已变化。
    Review { reason: String },
    /// 该内容已审核过：跳过，不跑任何评审。
    Skip { reason: String },
}

impl ContentDecision {
    /// The decision reason, already prefixed (`skipping: …` / `re-reviewing: …`).
    pub fn reason(&self) -> &str {
        match self {
            Self::Review { reason } | Self::Skip { reason } => reason,
        }
    }
}

/// Fingerprint of the diff text a review runs on (RENG-62).
///
/// SHA-256, hex-encoded, over the diff **exactly as the review consumes it** —
/// the string the provider client already returned and the experts are fed, so
/// the gate never issues a second `fetch_diff()`. Byte-exact: whitespace and
/// hunk order matter, because that is what the LLM sees.
pub fn content_fingerprint(diff: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(diff.as_bytes());
    hex::encode(hasher.finalize())
}

/// Pure decision of the unchanged-content gate: the noted content (SHA +
/// fingerprint) versus the content in hand.
///
/// | last fingerprint | fingerprint now | verdict |
/// |---|---|---|
/// | `None` (first sight, or a state file written before RENG-62) | any | [`ContentDecision::Review`] — fail-open, never a lost change |
/// | equal | equal | [`ContentDecision::Skip`] |
/// | different | any | [`ContentDecision::Review`] — the content moved, SHA change or not |
fn decide_content(
    last_sha: Option<&str>,
    last_fingerprint: Option<&str>,
    sha: &str,
    fingerprint: &str,
) -> ContentDecision {
    // The SHA is only shown in the reason; the fingerprint is what decides.
    let transition = match last_sha {
        Some(prev) if prev != sha => format!("{prev} → {sha}"),
        _ => sha.to_string(),
    };
    match last_fingerprint {
        None => ContentDecision::Review {
            reason: format!("re-reviewing: no reviewed content recorded (sha {sha})"),
        },
        Some(prev) if prev == fingerprint => ContentDecision::Skip {
            reason: format!("skipping: content unchanged (sha {transition})"),
        },
        Some(_) => ContentDecision::Review {
            reason: format!("re-reviewing: content changed (sha {transition})"),
        },
    }
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
    /// Missing in state files written before RENG-62: such an entry re-reviews
    /// once (fail-open) and records a fingerprint from then on.
    #[serde(default)]
    last_fingerprint: Option<String>,
}

impl MrDispatcher {
    /// In-memory dispatcher with the default 15-minute timeout and no
    /// persistence (used by tests and one-shot webhook handlers).
    pub fn new() -> Self {
        Self::with_state_file(None, Duration::from_secs(DEFAULT_TIMEOUT_SECS))
    }

    /// Dispatcher used by the long-running server: persists state to
    /// `REVIEW_DISPATCH_STATE` (default `<state dir>/dispatcher-state.json` —
    /// `~/.config/review-engine/` unless `serve --data-dir` moved the root)
    /// and honours `REVIEW_DISPATCH_TIMEOUT_SECS`.
    ///
    /// The resolved path is logged at startup so an operator can see where the
    /// dedup state lives — inside a container the default lands on the
    /// container filesystem and is lost on every recreate, which silently
    /// disarms the SHA guard (see `docs/configuration.md`).
    pub fn persistent() -> Self {
        let state_path = default_state_path();
        match state_path.as_deref() {
            Some(path) => tracing::info!("Dispatcher: persisting dispatch state to {}", path.display()),
            None => tracing::warn!(
                "Dispatcher: no dispatch state file (neither {STATE_PATH_ENV} nor a home directory is available) \
                 — reviewed SHAs will not survive a restart"
            ),
        }
        Self::with_state_file(state_path, configured_timeout())
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
            .or_insert_with(|| MrStatus::new(None, None, None));

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

    /// Content gate for a dispatch that already passed [`Self::try_start`]
    /// (RENG-62). `fingerprint` is [`content_fingerprint`] of the diff the
    /// review would run on — the caller already holds that diff, so the gate
    /// costs no extra provider call.
    ///
    /// On [`ContentDecision::Skip`] the dispatch is **finalized here**: the SHA
    /// is recorded (so the next event for it takes [`Self::try_start`]'s fast
    /// path), the running marker is cleared and waiters are notified. No review
    /// will run, and leaving the marker set would block the MR for a full
    /// `REVIEW_DISPATCH_TIMEOUT_SECS`. The recorded fingerprint is left
    /// untouched — it still describes the content on the MR.
    ///
    /// On [`ContentDecision::Review`] nothing is recorded: [`Self::complete`]
    /// owns that once the review actually finishes.
    pub async fn claim_content(&self, mr_url: &str, sha: &str, fingerprint: &str) -> ContentDecision {
        let mut map = self.inner.lock().await;
        let status = map
            .entry(mr_url.to_string())
            .or_insert_with(|| MrStatus::new(None, None, None));
        let decision = decide_content(
            status.last_sha.as_deref(),
            status.last_fingerprint.as_deref(),
            sha,
            fingerprint,
        );
        if matches!(decision, ContentDecision::Skip { .. }) {
            status.running_since = None;
            status.last_sha = Some(sha.to_string());
            status.signal_tx.send(true).ok();
            self.persist_locked(&map);
        }
        decision
    }

    /// 标记 review 完成，记录 SHA（以及本次审核内容的指纹），通知等待者。
    ///
    /// `fingerprint`: `Some(_)` records the content this review covered
    /// (RENG-62); `None` leaves any previously recorded fingerprint untouched.
    pub async fn complete(&self, mr_url: &str, sha: &str, fingerprint: Option<&str>) {
        let mut map = self.inner.lock().await;
        if let Some(status) = map.get_mut(mr_url) {
            status.running_since = None;
            status.last_sha = Some(sha.to_string());
            if let Some(fingerprint) = fingerprint {
                status.last_fingerprint = Some(fingerprint.to_string());
            }
            status.signal_tx.send(true).ok();
            self.persist_locked(&map);
        }
    }

    /// 等待当前 review 完成，最长等待一个 running-marker 超时周期
    /// （即 `self.timeout`，默认 15 分钟）。
    ///
    /// 等待被 `tokio::time::timeout` 强制上界：若 review task 被 abort、
    /// 永远不会调用 [`Self::complete`]/[`Self::reset`]，`wait` 也会在超时后
    /// 返回 [`WaitOutcome::TimedOut`]，而不是在 `rx.changed()` 上永久
    /// pending，从而保证 abort 后调用方线程/任务可回收（HIGH-1）。
    ///
    /// 超时取 `self.timeout` 而非更短的固定值：调用方的既有模式是
    /// `wait().await` 之后立即重新 `try_start`。以 `TimedOut` 返回时 running
    /// 标记恰好已经过期（`is_expired` 按 `>=` 判定），`try_start` 会返回 `Go`
    /// 走恢复路径；若用更短的固定超时（如 60s），健康但较慢的 review 尚未
    /// 结束时调用方会在 `try_start` 得到 `InProgress` 并丢弃本次延迟的
    /// review —— 属于回归。
    pub async fn wait(&self, mr_url: &str) -> WaitOutcome {
        let wait_timeout = self.timeout;
        let mut rx = {
            let map = self.inner.lock().await;
            match map.get(mr_url) {
                Some(status) if status.running_since.is_some() => Some(status.signal_rx.clone()),
                _ => None,
            }
        };

        let Some(ref mut rx) = rx else {
            // 没有正在运行的 review —— 无需等待。
            return WaitOutcome::Completed;
        };

        if *rx.borrow() {
            // complete()/reset() 在 wait() 开始监听之前已经发出信号。
            return WaitOutcome::Completed;
        }

        match tokio::time::timeout(wait_timeout, rx.changed()).await {
            // 正常完成：收到新信号。
            Ok(Ok(())) => WaitOutcome::Completed,
            // 发送端被 drop（条目被 remove() 移除或状态表销毁）：无可等待之物。
            Ok(Err(_)) => WaitOutcome::Completed,
            // 超时：review 未在超时周期内完成（如 task 被 abort），返回可识别状态。
            Err(_) => WaitOutcome::TimedOut,
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
                            last_fingerprint: status.last_fingerprint.clone(),
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

/// State file location: `REVIEW_DISPATCH_STATE` or `<state dir>/dispatcher-state.json`
/// (see [`crate::paths`]).
pub(crate) fn default_state_path() -> Option<PathBuf> {
    crate::paths::resolve_artifact(
        std::env::var(STATE_PATH_ENV).ok().as_deref(),
        crate::paths::DISPATCH_STATE_FILE_NAME,
    )
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
            (
                url,
                MrStatus::new(running_since, entry.last_sha, entry.last_fingerprint),
            )
        })
        .collect()
}

/// Serialize `state` to `path` atomically via a temp file + rename, so a
/// crash mid-write never leaves a truncated state file behind.
///
/// The parent directory is created when missing — a mounted state path such as
/// `/app/config/dispatcher-state.json` must work even when nothing has created
/// the directory yet.
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
        d.complete("mr1", "sha1", None).await;
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
        d.complete("mr1", "sha1", None).await;
        // new SHA after completion → Go
        assert_eq!(d.try_start("mr1", "sha2").await, ShouldStart::Go);
    }

    #[tokio::test]
    async fn test_wait_returns_immediately_when_not_running() {
        let d = MrDispatcher::new();
        // Not started yet — wait returns immediately with Completed
        assert_eq!(d.wait("mr1").await, WaitOutcome::Completed);
    }

    #[tokio::test]
    async fn test_wait_returns_after_complete() {
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);

        let d2 = d.clone();
        let handle = tokio::spawn(async move {
            d2.complete("mr1", "sha1", None).await;
        });

        // Wait should return Completed after complete is called
        assert_eq!(d.wait("mr1").await, WaitOutcome::Completed);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_wait_handles_early_completion() {
        // Edge case: complete() fires before wait() starts listening
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);

        // Signal before wait enters the await
        d.complete("mr1", "sha1", None).await;

        // wait() should see the signal was already sent and return Completed immediately
        assert_eq!(d.wait("mr1").await, WaitOutcome::Completed);
    }

    #[tokio::test]
    async fn test_wait_returns_timed_out_when_review_aborted() {
        // HIGH-1 regression test: the review task is aborted before it can call
        // complete()/reset(), so the signal is never sent and the running marker
        // is never cleared. wait() must NOT block forever on rx.changed().
        let d = MrDispatcher::with_state_file(None, Duration::from_millis(50));
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);

        // wait() must return within a bound with a distinguishable outcome.
        let outcome = tokio::time::timeout(Duration::from_secs(2), d.wait("mr1"))
            .await
            .expect("wait() must return within 2s even when the review is aborted");
        assert_eq!(outcome, WaitOutcome::TimedOut);

        // Recovery: after TimedOut the caller re-checks try_start, which sees the
        // expired marker and starts a new review.
        assert_eq!(d.try_start("mr1", "sha2").await, ShouldStart::Go);
    }

    #[tokio::test]
    async fn test_wait_returns_completed_when_entry_removed() {
        // The entry is removed while a waiter is listening: the watch sender is
        // dropped, so wait() must resolve (Completed), not hang.
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);

        let d2 = d.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            d2.remove("mr1").await;
        });

        assert_eq!(d.wait("mr1").await, WaitOutcome::Completed);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_remove_clears_entry() {
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
        d.complete("mr1", "sha1", None).await;
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

        d.complete("mr1", "sha1", None).await;

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
        d.complete("nonexistent", "sha1", None).await;
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
        d1.complete("mr1", "sha1", None).await;
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
        d1.complete("mr1", "sha1", None).await;
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
                last_fingerprint: Some("fp1".to_string()),
            },
        );
        write_state_atomic(&path, &state).unwrap();
        assert!(path.exists());
        assert!(!path.with_extension("tmp").exists());
    }

    // ─── RENG-62: mounted state path ────────────────────────────────

    /// `REVIEW_DISPATCH_STATE` wins verbatim; an empty value falls back to the
    /// state dir; no env/root at all disables persistence. The root chain
    /// (`--data-dir` → `REVIEW_ENGINE_CONFIG_DIR` → `~/.config/review-engine`)
    /// lives in [`crate::paths`], which owns its own table tests; this pins the
    /// dispatcher's wiring to it without touching process state.
    #[test]
    fn state_path_prefers_the_env_override_and_falls_back_to_the_state_dir() {
        let file_name = crate::paths::DISPATCH_STATE_FILE_NAME;
        assert_eq!(
            crate::paths::resolve_artifact_at(Some("/app/config/dispatcher-state.json"), file_name, None),
            Some(PathBuf::from("/app/config/dispatcher-state.json")),
            "a non-empty REVIEW_DISPATCH_STATE must win verbatim"
        );
        assert_eq!(
            crate::paths::resolve_artifact_at(Some(""), file_name, Some(PathBuf::from("/app/.config/review-engine"))),
            Some(PathBuf::from("/app/.config/review-engine/dispatcher-state.json")),
            "an empty REVIEW_DISPATCH_STATE must fall back to the state dir"
        );
        assert_eq!(
            crate::paths::state_dir_from(None, None, Some(PathBuf::from("/home/alice"))),
            Some(PathBuf::from("/home/alice/.config/review-engine")),
            "the non-container default must stay unchanged"
        );
        assert_eq!(
            crate::paths::resolve_artifact_at(None, file_name, None),
            None,
            "no env value and no state dir means no persistence"
        );
    }

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn new(key: &'static str) -> Self {
            Self {
                key,
                original: std::env::var(key).ok(),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.original.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    /// The state file is created, with its parent directory, when missing — the
    /// mounted `/app/config/...` path must work on a directory that has never
    /// held the file.
    #[tokio::test]
    async fn persistent_dispatcher_creates_the_configured_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config").join("dispatcher-state.json");
        let _guard = EnvGuard::new(STATE_PATH_ENV);
        std::env::set_var(STATE_PATH_ENV, &path);

        let d = MrDispatcher::persistent();
        assert!(
            !path.exists(),
            "the state file must not exist before the first dispatch"
        );
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
        d.complete("mr1", "sha1", Some("fp1")).await;

        let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["entries"]["mr1"]["last_sha"], "sha1");
        assert_eq!(json["entries"]["mr1"]["last_fingerprint"], "fp1");
    }

    // ─── RENG-62: content-based gating ─────────────────────────────

    #[test]
    fn content_fingerprint_is_stable_and_content_sensitive() {
        assert_eq!(content_fingerprint("diff text"), content_fingerprint("diff text"));
        assert_eq!(content_fingerprint("diff text").len(), 64, "hex-encoded SHA-256");
        assert_ne!(content_fingerprint("diff text"), content_fingerprint("diff text\n"));
        assert_ne!(content_fingerprint("a"), content_fingerprint("b"));
    }

    /// The decision table: first sight reviews; identical content skips;
    /// changed content reviews.
    #[test]
    fn decide_content_covers_first_sight_unchanged_and_changed() {
        // First sight (no fingerprint recorded) → review, fail-open.
        assert!(matches!(
            decide_content(None, None, "sha1", "fpA"),
            ContentDecision::Review { .. }
        ));

        // Same SHA, same content → unchanged.
        let same = decide_content(Some("sha1"), Some("fpA"), "sha1", "fpA");
        assert_eq!(
            same,
            ContentDecision::Skip {
                reason: "skipping: content unchanged (sha sha1)".to_string()
            }
        );

        // Amend / force-push: new SHA, byte-identical content → unchanged.
        let amend = decide_content(Some("sha1"), Some("fpA"), "sha2", "fpA");
        assert_eq!(
            amend,
            ContentDecision::Skip {
                reason: "skipping: content unchanged (sha sha1 → sha2)".to_string()
            }
        );

        // New SHA, different content → review, naming the transition.
        let changed = decide_content(Some("sha1"), Some("fpA"), "sha2", "fpB");
        assert_eq!(
            changed,
            ContentDecision::Review {
                reason: "re-reviewing: content changed (sha sha1 → sha2)".to_string()
            }
        );

        // Force-push back to an older state → the content differs from what was
        // reviewed, so it reviews (and then records the old content as current).
        assert!(matches!(
            decide_content(Some("sha2"), Some("fpA"), "sha1", "fpB"),
            ContentDecision::Review { .. }
        ));
    }

    /// Skip path: the dispatch is finalized (SHA recorded, running marker
    /// cleared) so the same event cannot re-enter the gate or wedge the MR.
    #[tokio::test]
    async fn claim_content_skips_unchanged_content_and_finalizes_dispatch() {
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
        d.complete("mr1", "sha1", Some("fpA")).await;

        // Amend: new SHA, identical diff.
        assert_eq!(d.try_start("mr1", "sha2").await, ShouldStart::Go);
        let decision = d.claim_content("mr1", "sha2", "fpA").await;
        assert_eq!(
            decision,
            ContentDecision::Skip {
                reason: "skipping: content unchanged (sha sha1 → sha2)".to_string()
            }
        );
        assert_eq!(decision.reason(), "skipping: content unchanged (sha sha1 → sha2)");

        // The skipped SHA is recorded: its next event takes the fast SHA path.
        assert_eq!(d.try_start("mr1", "sha2").await, ShouldStart::AlreadyReviewed);
        // The running marker was cleared: a genuinely new SHA can go, not InProgress.
        assert_eq!(d.try_start("mr1", "sha3").await, ShouldStart::Go);
    }

    /// Changed content with a new SHA re-reviews (and nothing is recorded until
    /// the review completes).
    #[tokio::test]
    async fn claim_content_reviews_when_content_changed() {
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
        d.complete("mr1", "sha1", Some("fpA")).await;

        assert_eq!(d.try_start("mr1", "sha2").await, ShouldStart::Go);
        assert_eq!(
            d.claim_content("mr1", "sha2", "fpB").await,
            ContentDecision::Review {
                reason: "re-reviewing: content changed (sha sha1 → sha2)".to_string()
            }
        );
        // Still running: the review owns the entry until complete().
        assert_eq!(d.try_start("mr1", "sha2").await, ShouldStart::InProgress);
    }

    /// A first-sight MR is never skipped by the gate, even when the caller has
    /// no fingerprint to compare (fresh MR, or a pre-RENG-62 state file).
    #[tokio::test]
    async fn claim_content_never_skips_a_first_sight() {
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
        assert!(matches!(
            d.claim_content("mr1", "sha1", "fpA").await,
            ContentDecision::Review { .. }
        ));
    }

    /// Container-recreation regression: the fingerprint survives a reload, so
    /// the amend that follows a restart is still recognized as unchanged.
    #[tokio::test]
    async fn fingerprint_survives_reload_and_gates_amends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dispatcher-state.json");

        let d1 = MrDispatcher::with_state_file(Some(path.clone()), Duration::from_secs(3600));
        assert_eq!(d1.try_start("mr1", "sha1").await, ShouldStart::Go);
        d1.complete("mr1", "sha1", Some("fpA")).await;
        drop(d1);

        let d2 = MrDispatcher::with_state_file(Some(path), Duration::from_secs(3600));
        assert_eq!(d2.try_start("mr1", "sha1").await, ShouldStart::AlreadyReviewed);
        assert_eq!(d2.try_start("mr1", "sha2").await, ShouldStart::Go);
        assert!(matches!(
            d2.claim_content("mr1", "sha2", "fpA").await,
            ContentDecision::Skip { .. }
        ));
    }

    /// A state file written before RENG-62 carries a SHA but no fingerprint:
    /// the next event reviews once (never a lost change) and records one.
    #[tokio::test]
    async fn legacy_state_without_fingerprint_reviews_once_then_records_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dispatcher-state.json");
        std::fs::write(&path, r#"{"entries":{"mr1":{"last_sha":"sha1","running_since":null}}}"#).unwrap();

        let d = MrDispatcher::with_state_file(Some(path.clone()), Duration::from_secs(3600));
        assert_eq!(d.try_start("mr1", "sha2").await, ShouldStart::Go);
        assert!(matches!(
            d.claim_content("mr1", "sha2", "fpA").await,
            ContentDecision::Review { .. }
        ));
        d.complete("mr1", "sha2", Some("fpA")).await;

        let d2 = MrDispatcher::with_state_file(Some(path), Duration::from_secs(3600));
        assert_eq!(d2.try_start("mr1", "sha3").await, ShouldStart::Go);
        assert!(matches!(
            d2.claim_content("mr1", "sha3", "fpA").await,
            ContentDecision::Skip { .. }
        ));
    }

    /// `complete(..., None)` (callers without content information) leaves the
    /// recorded fingerprint in place rather than erasing it.
    #[tokio::test]
    async fn complete_without_fingerprint_keeps_the_recorded_one() {
        let d = MrDispatcher::new();
        assert_eq!(d.try_start("mr1", "sha1").await, ShouldStart::Go);
        d.complete("mr1", "sha1", Some("fpA")).await;
        assert_eq!(d.try_start("mr1", "sha2").await, ShouldStart::Go);
        d.complete("mr1", "sha2", None).await;

        assert_eq!(d.try_start("mr1", "sha3").await, ShouldStart::Go);
        assert!(matches!(
            d.claim_content("mr1", "sha3", "fpA").await,
            ContentDecision::Skip { .. }
        ));
    }
}
