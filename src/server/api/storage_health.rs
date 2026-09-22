//! Cached storage-writability health (RENG-106).
//!
//! The Dashboard's 「存储」 row and `GET /system/health` both read THIS store,
//! so the two surfaces cannot disagree — the same single-source rule the
//! integration rows follow (RENG-97).
//!
//! # Why a real write test
//!
//! A host-side process can change the owner of `review.db-wal` /
//! `review.db-shm` (or of the config dir) out from under a running container.
//! SQLite in WAL mode then cannot write its sidecars, and **every** write
//! fails with `attempt to write a readonly database` while reviews keep
//! running and keep posting to GitLab — the incident behind RENG-106. Nothing
//! observable from the outside says so: the file exists, its mode looks fine,
//! and the API answers. The only honest verdict is therefore a real,
//! content-preserving write (`PRAGMA user_version = <its current value>`):
//! it dirties page 1 and commits, so a database that cannot be written replies
//! with sqlite's own words.
//!
//! The probe opens a FRESH connection — the very
//! [`crate::doctor::write_test`] `reng doctor` runs — rather than reusing the
//! server's pool. A long-running process holds its WAL and `-shm` descriptors
//! open, so it can go on reporting `saved` into unlinked inodes long after no
//! new connection can write; the on-disk state is both the user-visible truth
//! and the verdict the CLI and the UI therefore share.
//!
//! # Why it is cached
//!
//! The dashboard polls every 60 s per open tab. The probe writes to the WAL,
//! so it must NOT run per request: the last verdict is kept in
//! [`crate::server::AppState`] and re-probed only when it is older than
//! [`DEFAULT_TTL`] (or was never taken). One write per minute, whatever the
//! page does.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::doctor;
use crate::server::AppState;
use crate::store::BackendKind;

/// How long one verdict is served before the next reader re-probes.
///
/// 60 s keeps the probe's WAL write to one per minute however many tabs poll,
/// while a storage failure surfaces within a poll cycle of happening.
pub const DEFAULT_TTL: Duration = Duration::from_secs(60);

/// The wire vocabulary of a storage verdict — the shape `/system/health` and
/// the dashboard payload carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageStatus {
    /// The write test went through.
    Healthy,
    /// The write test failed (the database is losing writes).
    Error,
}

impl StorageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Error => "error",
        }
    }
}

/// One storage verdict, as of its last probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageHealth {
    pub status: StorageStatus,
    /// `Write test passed` when healthy; sqlite's own error plus the
    /// ownership/mode cause when it failed, so the row explains *why*.
    pub message: String,
    /// When the probe behind this verdict ran. Served to the UI, so a verdict
    /// older than the TTL is at least visible as such.
    pub checked_at: DateTime<Utc>,
}

impl StorageHealth {
    pub fn healthy(message: impl Into<String>) -> Self {
        Self {
            status: StorageStatus::Healthy,
            message: message.into(),
            checked_at: Utc::now(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            status: StorageStatus::Error,
            message: message.into(),
            checked_at: Utc::now(),
        }
    }

    pub fn is_error(&self) -> bool {
        self.status == StorageStatus::Error
    }

    /// The `storage` entry of `/system/health` and of the dashboard's `health`
    /// object: `{status, message, checkedAt}`.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "status": self.status.as_str(),
            "message": self.message,
            "checkedAt": self.checked_at.to_rfc3339(),
        })
    }
}

/// What a storage probe targets.
#[derive(Clone)]
pub enum StorageTarget {
    /// The embedded SQLite FILE the store is backed by. Probed with a FRESH
    /// connection (the CLI doctor's own [`doctor::write_test`]), deliberately
    /// not through the live pool: a running process holds its WAL and `-shm`
    /// file descriptors open, so it can keep answering `saved` into unlinked
    /// inodes while every new connection — the next restart, a backup tool,
    /// `reng doctor` — cannot write at all. The file's state is the verdict
    /// the user needs, and it is the one both surfaces agree on.
    SqliteFile(PathBuf),
    /// SQLite in memory (`sqlite::memory:`): no file, nothing to lose.
    Memory,
    /// PostgreSQL (`DATABASE_URL`): connectivity is the pool's business and
    /// there is no WAL sidecar to lose.
    Postgres,
    /// No database attached (`REVIEW_DISABLE_DB=1`, tests, embedded use):
    /// nothing is persisted, so nothing can fail to persist.
    Disabled,
}

impl std::fmt::Debug for StorageTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SqliteFile(path) => write!(f, "SqliteFile({})", path.display()),
            Self::Memory => f.write_str("Memory"),
            Self::Postgres => f.write_str("Postgres"),
            Self::Disabled => f.write_str("Disabled"),
        }
    }
}

/// A probe, injectable so tests can force a verdict without a broken store.
pub type ProbeFn = Arc<dyn Fn(StorageTarget) -> Pin<Box<dyn Future<Output = StorageHealth> + Send>> + Send + Sync>;

/// The cached storage verdict, shared through [`AppState`].
pub struct StorageHealthStore {
    ttl: chrono::Duration,
    probe: ProbeFn,
    cached: RwLock<Option<StorageHealth>>,
}

impl StorageHealthStore {
    /// The production store: a real write test against the attached pool, one
    /// per [`DEFAULT_TTL`].
    pub fn new() -> Self {
        Self::with_probe(
            Arc::new(|target| Box::pin(probe_target(target)) as Pin<Box<dyn Future<Output = _> + Send>>),
            DEFAULT_TTL,
        )
    }

    /// Test seam: probe with `probe`, serve verdicts for `ttl`.
    pub fn with_probe(probe: ProbeFn, ttl: Duration) -> Self {
        Self {
            ttl: chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::seconds(60)),
            probe,
            cached: RwLock::new(None),
        }
    }

    /// The verdict for `target`: the cached one while it is fresh, otherwise a
    /// fresh probe. Never more than one real write test per TTL.
    pub async fn report(&self, target: StorageTarget) -> StorageHealth {
        if let Some(verdict) = self.cached() {
            return verdict;
        }
        let verdict = (self.probe)(target).await;
        *self.cached.write().unwrap_or_else(|e| e.into_inner()) = Some(verdict.clone());
        verdict
    }

    /// Record a verdict that already ran (tests, and any future explicit
    /// re-probe path), so the next read agrees with it.
    pub fn record(&self, verdict: StorageHealth) {
        *self.cached.write().unwrap_or_else(|e| e.into_inner()) = Some(verdict);
    }

    fn cached(&self) -> Option<StorageHealth> {
        let entry = self.cached.read().unwrap_or_else(|e| e.into_inner()).clone()?;
        let fresh = Utc::now().signed_duration_since(entry.checked_at) < self.ttl;
        fresh.then_some(entry)
    }
}

impl Default for StorageHealthStore {
    fn default() -> Self {
        Self::new()
    }
}

/// The probe target for `state`'s store.
pub fn target_for(state: &AppState) -> StorageTarget {
    match state.db.as_ref() {
        None => StorageTarget::Disabled,
        Some(store) => match store.backend_kind() {
            BackendKind::Postgresql => StorageTarget::Postgres,
            BackendKind::Sqlite if store.is_in_memory() => StorageTarget::Memory,
            BackendKind::Sqlite => match doctor::sqlite_db_path() {
                Some(path) => StorageTarget::SqliteFile(path),
                // A SQLite store with no resolvable state dir: nothing on disk
                // to diagnose, so the honest answer is "not persisted".
                None => StorageTarget::Disabled,
            },
        },
    }
}

/// The storage verdict for `state`, from the shared cache.
pub async fn storage_health(state: &AppState) -> StorageHealth {
    state.storage_health.report(target_for(state)).await
}

/// Fold the storage verdict into a panel's `overall`: a database that cannot be
/// written means the deployment is already losing reviews, which no other row
/// can outrank.
pub fn overall_with_storage(overall: &'static str, storage: &StorageHealth) -> &'static str {
    if storage.is_error() {
        "error"
    } else {
        overall
    }
}

async fn probe_target(target: StorageTarget) -> StorageHealth {
    match target {
        StorageTarget::Disabled => StorageHealth::healthy("Persistence disabled (no database attached)"),
        StorageTarget::Memory => StorageHealth::healthy("SQLite in memory"),
        // A pool that reached PostgreSQL is serving; its own health is not a
        // file-permission question, and there is no WAL sidecar to lose.
        StorageTarget::Postgres => StorageHealth::healthy("PostgreSQL (no WAL sidecar)"),
        StorageTarget::SqliteFile(path) => {
            // The ownership/mode cause is read BEFORE the probe: a failing
            // write may itself replace the sidecar it could not use (SQLite
            // deletes and recreates an unusable `-shm`), which would erase the
            // evidence — `attempt to write a readonly database` alone never
            // says which file is the problem, which is what the incident
            // needed.
            let cause = doctor::unwritable_cause(&path);
            match doctor::write_test(&path).await {
                Ok(()) => StorageHealth::healthy("Write test passed"),
                Err(message) => StorageHealth::error(match cause {
                    Some(cause) => format!("{message}（{cause}）"),
                    None => message,
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn counting_store(verdict: StorageHealth, ttl: Duration) -> (Arc<StorageHealthStore>, Arc<AtomicU32>) {
        let calls = Arc::new(AtomicU32::new(0));
        let counter = calls.clone();
        let store = StorageHealthStore::with_probe(
            Arc::new(move |_target| {
                let verdict = verdict.clone();
                let counter = counter.clone();
                Box::pin(async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    verdict
                }) as Pin<Box<dyn Future<Output = _> + Send>>
            }),
            ttl,
        );
        (Arc::new(store), calls)
    }

    /// The 60 s cache is the whole reason the health poll does not write to the
    /// WAL on every request: the second read is served the first verdict.
    #[tokio::test]
    async fn a_fresh_verdict_is_served_without_probing_again() {
        let (store, calls) = counting_store(StorageHealth::healthy("Write test passed"), DEFAULT_TTL);
        for _ in 0..5 {
            let verdict = store.report(StorageTarget::Disabled).await;
            assert_eq!(verdict.status, StorageStatus::Healthy);
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "five polls inside the TTL must share one probe"
        );
    }

    /// A stale verdict is re-probed — the status is never allowed to go
    /// permanently stale.
    #[tokio::test]
    async fn a_stale_verdict_is_re_probed() {
        let (store, calls) = counting_store(
            StorageHealth::error("attempt to write a readonly database"),
            Duration::ZERO,
        );
        store.report(StorageTarget::Disabled).await;
        store.report(StorageTarget::Disabled).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// The JSON shape the frontend reads.
    #[test]
    fn the_entry_is_the_documented_shape() {
        let verdict = StorageHealth::error("attempt to write a readonly database");
        let json = verdict.to_json();
        assert_eq!(json["status"], "error");
        assert_eq!(json["message"], "attempt to write a readonly database");
        assert!(json["checkedAt"].as_str().is_some_and(|at| at.contains('T')));
        assert_eq!(
            StorageHealth::healthy("Write test passed").to_json()["status"],
            "healthy"
        );
    }

    /// An unprobed state reads the store's probe result, and a state with no
    /// database at all is healthy rather than alarming: nothing is persisted,
    /// so no write can fail.
    #[tokio::test]
    async fn a_state_without_a_database_is_healthy() {
        let state = AppState::new(vec![]);
        assert!(matches!(target_for(&state), StorageTarget::Disabled));
        let verdict = storage_health(&state).await;
        assert_eq!(verdict.status, StorageStatus::Healthy, "{verdict:?}");
        assert!(verdict.message.contains("disabled"), "{verdict:?}");
    }

    /// A SQLite store with no file behind it (tests, embedded use) is reported
    /// as what it is — in memory — instead of blaming a path that was never
    /// its database.
    #[tokio::test]
    async fn an_in_memory_store_is_not_probed_as_a_file() {
        let mut state = AppState::new(vec![]);
        state.db = Some(Arc::new(crate::store::SqlxStore::new_in_memory().await.unwrap()));
        assert!(matches!(target_for(&state), StorageTarget::Memory));
        let verdict = storage_health(&state).await;
        assert_eq!(verdict.status, StorageStatus::Healthy, "{verdict:?}");
        assert!(verdict.message.contains("memory"), "{verdict:?}");
    }

    /// The file probe is a REAL write test, with the incident's own failure
    /// text: a writable database passes, and one whose sidecars this process
    /// cannot write reads `error` carrying sqlite's words plus the cause.
    #[tokio::test]
    async fn the_file_probe_is_a_real_write_test() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;
        // Root bypasses the mode bits this test relies on.
        if doctor::current_uid() == Some(0) {
            eprintln!("skipped: root bypasses the mode bits this test relies on");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join(crate::paths::DB_FILE_NAME);
        ::sqlx::any::install_default_drivers();
        let pool = ::sqlx::any::AnyPoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{}?mode=rwc", db.display()))
            .await
            .unwrap();
        sqlx::query("PRAGMA journal_mode = WAL").fetch_all(&pool).await.unwrap();
        sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let healthy = probe_target(StorageTarget::SqliteFile(db.clone())).await;
        assert_eq!(healthy.status, StorageStatus::Healthy, "{healthy:?}");
        assert_eq!(healthy.message, "Write test passed");

        // Break it the way the incident did: sidecars this process cannot
        // write (a different owner is the same verdict, and needs root).
        for suffix in doctor::SIDECAR_SUFFIXES {
            let path = doctor::sidecar_path(&db, suffix);
            std::fs::write(&path, b"").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        }
        let broken = probe_target(StorageTarget::SqliteFile(db.clone())).await;
        assert_eq!(
            broken.status,
            StorageStatus::Error,
            "an unwritable database must not read healthy: {broken:?}"
        );
        assert!(
            broken.message.contains("review.db-"),
            "the cause names the file: {}",
            broken.message
        );
        assert!(
            broken.message.contains("不允许写入"),
            "the cause says why: {}",
            broken.message
        );
        assert!(
            broken.message.contains("mode 0444"),
            "the cause names the mode: {}",
            broken.message
        );
        assert!(
            !broken.message.contains("error returned from database"),
            "sqlx's wrapper is stripped: {}",
            broken.message
        );
    }

    /// The fold: an error anywhere makes the panel `error`, and a healthy
    /// storage never upgrades a worse LLM verdict.
    #[test]
    fn overall_degrades_on_storage_error_but_never_upgrades() {
        let ok = StorageHealth::healthy("Write test passed");
        let bad = StorageHealth::error("attempt to write a readonly database");
        assert_eq!(overall_with_storage("success", &ok), "success");
        assert_eq!(overall_with_storage("warning", &ok), "warning");
        assert_eq!(overall_with_storage("offline", &ok), "offline");
        for llm in ["success", "warning", "error", "offline"] {
            assert_eq!(
                overall_with_storage(llm, &bad),
                "error",
                "a database losing writes must not read '{llm}'"
            );
        }
    }
}
