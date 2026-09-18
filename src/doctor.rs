//! Storage diagnosis and self-repair: `reng doctor [--fix]` (RENG-106), plus
//! the checks the System-Health 存储 row and the container entrypoint's
//! startup self-heal (RENG-105) are built from.
//!
//! # The incident this exists for
//!
//! A host-side process on the user's NAS changed the owner of
//! `review.db-wal` / `review.db-shm` to a uid other than the container's app
//! user (9001). SQLite in WAL mode could then no longer write its sidecars, so
//! **every** database write failed with `attempt to write a readonly database`:
//! reviews ran and posted to GitLab but never persisted, and the UI gave no
//! hint at all. An ordinary user cannot diagnose that from the Web UI and
//! certainly cannot repair it.
//!
//! # What the doctor can and cannot do
//!
//! Deleting the sidecars is rootless: `unlink` needs write permission on the
//! **directory**, never on the file, so the app user can remove sidecars owned
//! by another uid. SQLite recreates both with the current uid on the next
//! write. That is the whole repair — no `chown`, no `chmod`, and **never** a
//! touch of `review.db` itself (its content is the user's review history).
//! When the config dir itself is not writable, or `review.db` is the
//! unwritable file, nothing here can help: that needs root / the host.
//!
//! # Why the checks are shaped this way
//!
//! [`write_test`] is the only authoritative verdict: it performs a real,
//! content-preserving write (`PRAGMA user_version = <its current value>` — a
//! write transaction that changes no byte of meaning), so a database that
//! cannot be written answers with sqlite's own words. The ownership/mode
//! checks around it exist to *explain* a failure, because
//! `attempt to write a readonly database` never says which file is the
//! problem. [`plan_fix`] is a pure function of an [`Diagnosis`], so every
//! decision — repair, refuse, escalate — is unit-testable without root.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::paths::DB_FILE_NAME;

/// Suffixes SQLite appends to the database file in WAL mode, in the order the
/// report lists them.
pub const SIDECAR_SUFFIXES: [&str; 2] = ["-wal", "-shm"];

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Stable id (`config-dir`, `review.db`, `review.db-wal`, `write-test`),
    /// used to select a check in tests and code.
    pub id: String,
    /// True when the check found nothing wrong.
    pub ok: bool,
    /// True when the check could not be evaluated — the write test cannot run
    /// without a `review.db`. A skipped check is not a failure and never
    /// drives the exit code, but it does mean "not verified".
    pub skipped: bool,
    /// Complete human line, self-contained: for a failure, the exact cause.
    pub detail: String,
}

impl Check {
    fn pass(id: &str, detail: String) -> Self {
        Self {
            id: id.to_string(),
            ok: true,
            skipped: false,
            detail,
        }
    }

    fn fail(id: &str, detail: String) -> Self {
        Self {
            id: id.to_string(),
            ok: false,
            skipped: false,
            detail,
        }
    }

    fn skipped(id: &str, detail: String) -> Self {
        Self {
            id: id.to_string(),
            ok: false,
            skipped: true,
            detail,
        }
    }

    /// The `[PASS]` / `[FAIL]` / `[SKIP]` column.
    pub fn tag(&self) -> &'static str {
        if self.skipped {
            "[SKIP]"
        } else if self.ok {
            "[PASS]"
        } else {
            "[FAIL]"
        }
    }
}

/// What the filesystem says about one file the diagnosis inspects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileState {
    pub path: PathBuf,
    pub exists: bool,
    /// Owner uid / gid; `None` on a platform without them (Windows).
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    /// Permission bits, as in `0o644`.
    pub mode: Option<u32>,
    pub size: u64,
    /// The open-for-write syscall succeeded. Mode arithmetic is deliberately
    /// NOT used: supplementary groups and root make it wrong, and the question
    /// that matters is whether *this* process can write.
    pub writable: bool,
}

impl FileState {
    /// Inspect `path`. A missing file is a state of its own, never an error.
    pub fn read(path: &Path) -> Self {
        match fs::metadata(path) {
            Ok(meta) => {
                let (uid, gid, mode) = ownership_of(&meta);
                Self {
                    path: path.to_path_buf(),
                    exists: true,
                    uid,
                    gid,
                    mode,
                    size: meta.len(),
                    writable: writable_by_this_process(path),
                }
            }
            Err(_) => Self {
                path: path.to_path_buf(),
                exists: false,
                uid: None,
                gid: None,
                mode: None,
                size: 0,
                writable: false,
            },
        }
    }

    /// The file's base name, as the messages name it (`review.db-wal`).
    fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    /// `属主 uid=9001 gid=9001 mode=0644` — omitted bits say `?`.
    fn ownership_line(&self) -> String {
        let uid = self.uid.map(|u| u.to_string()).unwrap_or_else(|| "?".to_string());
        let gid = self.gid.map(|g| g.to_string()).unwrap_or_else(|| "?".to_string());
        let mode = self.mode.map(|m| format!("{m:04o}")).unwrap_or_else(|| "?".to_string());
        format!("属主 uid={uid} gid={gid} mode={mode}")
    }
}

/// The whole diagnosis: the check lines plus the raw facts [`plan_fix`]
/// decides on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    pub dir: PathBuf,
    /// The config dir accepted a create+remove probe file.
    pub dir_writable: bool,
    pub checks: Vec<Check>,
    pub db: FileState,
    /// Sidecars that exist right now, `-wal` first.
    pub sidecars: Vec<FileState>,
    /// The real write test: `None` when it could not run (no `review.db`).
    pub write_test: Option<Result<(), String>>,
}

impl Diagnosis {
    /// True when no check failed. A skip is not a failure (a missing
    /// `review.db` already fails its own check).
    pub fn healthy(&self) -> bool {
        self.checks.iter().all(|c| c.ok || c.skipped)
    }

    /// The check with `id`, if any.
    pub fn check(&self, id: &str) -> Option<&Check> {
        self.checks.iter().find(|c| c.id == id)
    }

    /// `review.db-wal` when it exists and holds bytes.
    fn non_empty_wal(&self) -> Option<&FileState> {
        self.sidecars.iter().find(|s| s.name().ends_with("-wal") && s.size > 0)
    }
}

/// What `--fix` will do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixPlan {
    /// Nothing to repair: the write test passed, or it failed for a reason
    /// that deleting sidecars cannot fix (`review.db` itself is unwritable, or
    /// the sidecars are writable and something else is wrong).
    Nothing,
    /// Delete these sidecars, then re-run the write test.
    DeleteSidecars(Vec<PathBuf>),
    /// `-wal` holds bytes: deleting it can drop committed-but-uncheckpointed
    /// transactions. Refuse and ask for a graceful stop first.
    RefuseNonEmptyWal { path: PathBuf, size: u64 },
    /// The config dir itself is not writable, so nothing can be deleted. Only
    /// root / the host can repair it.
    NeedsPrivileges { dir: PathBuf },
}

/// Inspect the storage under `dir`. Creates and removes one probe file in the
/// config dir; never writes to `review.db` beyond the write test's
/// content-preserving `PRAGMA`.
pub async fn inspect(dir: &Path) -> Diagnosis {
    let db = dir.join(DB_FILE_NAME);
    let dir_writable = probe_dir(dir);
    let db_state = FileState::read(&db);
    let sidecars: Vec<FileState> = SIDECAR_SUFFIXES
        .iter()
        .map(|suffix| FileState::read(&sidecar_path(&db, suffix)))
        .filter(|state| state.exists)
        .collect();

    // The definitive verdict runs AFTER the checks are captured, on purpose:
    // it is a real write, so SQLite may delete and recreate sidecars while it
    // runs (it does exactly that when it finds an unusable one — the recreate
    // is mode 0644, and the connection still fails, because its readonly
    // verdict is latched for the connection's lifetime). The report must name
    // the files that were blocking, which is the state BEFORE the probe.
    // Skipped without a database: there is nothing to write to, and the
    // missing file is already reported above.
    let write_test = if db_state.exists {
        Some(write_test(&db).await)
    } else {
        None
    };

    let mut checks = vec![if dir_writable {
        Check::pass(
            "config-dir",
            format!("config-dir：{} 存在且当前进程可写", dir.display()),
        )
    } else {
        Check::fail(
            "config-dir",
            format!("config-dir：{} 当前进程不可写（无法在其中创建临时文件）", dir.display()),
        )
    }];

    checks.push(if !db_state.exists {
        Check::fail(
            "review.db",
            format!("review.db：{} 不存在（`reng serve` 首次写入时会创建）", db.display()),
        )
    } else if db_state.writable {
        Check::pass(
            "review.db",
            format!("review.db：{}，当前进程可写", db_state.ownership_line()),
        )
    } else {
        Check::fail("review.db", write_cause(&db_state))
    });

    for state in &sidecars {
        let id = state.name();
        checks.push(if state.writable {
            Check::pass(&id, format!("{id}：{}，当前进程可写", state.ownership_line()))
        } else {
            Check::fail(&id, write_cause(state))
        });
    }

    match &write_test {
        Some(Err(message)) => checks.push(Check::fail("write-test", format!("write-test：{message}"))),
        Some(Ok(())) => checks.push(Check::pass(
            "write-test",
            "write-test：写入测试通过（PRAGMA user_version 未变）".to_string(),
        )),
        None => checks.push(Check::skipped(
            "write-test",
            "write-test：review.db 不存在，跳过写入测试".to_string(),
        )),
    }

    Diagnosis {
        dir: dir.to_path_buf(),
        dir_writable,
        checks,
        db: db_state,
        sidecars,
        write_test,
    }
}

/// Decide the repair from a diagnosis alone — no filesystem access, so every
/// branch is testable without root and without setting up a broken store.
///
/// The repair is only ever the sidecars. An unwritable `review.db` cannot be
/// fixed by deleting anything (the file must keep its data and only root can
/// `chown` it), so that case is refused rather than "repaired" into data loss.
pub fn plan_fix(diagnosis: &Diagnosis) -> FixPlan {
    // Only a FAILING write test needs a repair: a database SQLite just wrote
    // through is fine whatever the sidecars' owner says.
    match &diagnosis.write_test {
        None | Some(Ok(())) => return FixPlan::Nothing,
        Some(Err(_)) => {}
    }
    if !diagnosis.dir_writable {
        return FixPlan::NeedsPrivileges {
            dir: diagnosis.dir.clone(),
        };
    }
    // "(and only if) the write test fails because a sidecar is not writable":
    // no blocked sidecar means the cause is elsewhere, and deleting files
    // would be an unprovoked mutation.
    if diagnosis.sidecars.iter().all(|state| state.writable) {
        return FixPlan::Nothing;
    }
    // Safety gate: a `-wal` with content may hold committed transactions that
    // were never checkpointed into `review.db`. Deleting it can lose them.
    if let Some(wal) = diagnosis.non_empty_wal() {
        return FixPlan::RefuseNonEmptyWal {
            path: wal.path.clone(),
            size: wal.size,
        };
    }
    // Every existing sidecar goes, not only the blocked one: SQLite recreates
    // both on the next write, and a half-fixed pair (say a stale writable
    // `-shm` against a fresh `-wal`) is worse than none.
    FixPlan::DeleteSidecars(diagnosis.sidecars.iter().map(|state| state.path.clone()).collect())
}

/// `reng doctor`: diagnose the resolved config dir, repair it with `--fix`,
/// print the report and return the process exit code (0 = healthy, or repaired
/// successfully).
///
/// `quiet` (the entrypoint's startup self-heal, RENG-105) prints nothing while
/// every check passes and reports only the failures otherwise — a boot log
/// does not need four green lines per start.
pub async fn run(fix: bool, quiet: bool) -> i32 {
    let Some(dir) = crate::paths::state_dir() else {
        eprintln!("reng doctor：无法解析配置目录（REVIEW_ENGINE_CONFIG_DIR 与 HOME 都不可用）");
        return 1;
    };
    let diagnosis = inspect(&dir).await;
    let mut lines: Vec<String> = Vec::new();
    let mut verdict: Vec<String> = Vec::new();
    let mut code = if diagnosis.healthy() { 0 } else { 1 };
    lines.push(format!("reng doctor — 存储自检：{}", dir.display()));
    for check in &diagnosis.checks {
        lines.push(format!("{} {}", check.tag(), check.detail));
    }

    let mut repaired = false;
    if fix {
        match plan_fix(&diagnosis) {
            FixPlan::Nothing => {
                if diagnosis.healthy() {
                    verdict.push("存储自检通过：无需修复。".to_string());
                } else if !diagnosis.db.exists {
                    verdict.push(format!(
                        "无需修复：{} 尚不存在（`reng serve` 首次写入时会创建）；\
                         若该部署本该有数据，请检查配置目录（REVIEW_ENGINE_CONFIG_DIR）是否指向正确的卷。",
                        diagnosis.db.path.display()
                    ));
                    code = 1;
                } else {
                    verdict.push(
                        "无法自动修复：写入失败的原因不是 WAL sidecar（review.db 本身不可写），\
                         需要 root 或宿主机修复文件属主。"
                            .to_string(),
                    );
                    code = 1;
                }
            }
            FixPlan::NeedsPrivileges { dir } => {
                verdict.push(format!(
                    "无法自动修复：配置目录 {} 当前进程不可写，需要 root 或宿主机权限修复目录属主。",
                    dir.display()
                ));
                code = 1;
            }
            FixPlan::RefuseNonEmptyWal { path, size } => {
                verdict.push(format!(
                    "拒绝删除非空的 {}（{size} 字节，可能含未提交事务）：请先优雅停止 reng\
                     （docker stop / 停止 serve），再重跑 `reng doctor --fix`。若停止后该文件仍然非空\
                     （它当前不可写，SQLite 无法 checkpoint 掉它），就只能由 root / 宿主机修复它的属主——\
                     自愈不会为了恢复写入而丢掉已提交的数据。",
                    path.display()
                ));
                code = 1;
            }
            FixPlan::DeleteSidecars(paths) => {
                let names: Vec<String> = paths
                    .iter()
                    .map(|p| {
                        p.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| p.display().to_string())
                    })
                    .collect();
                match delete_sidecars(&paths, &diagnosis.db.path).await {
                    Ok(()) => {
                        repaired = true;
                        code = 0;
                        verdict.push(format!(
                            "已删除 {}（删除只依赖配置目录的写权限，与文件属主无关）；\
                             SQLite 会在下一次写入时以当前 uid 重建 sidecar，正在运行的 serve 无需重启即可恢复写入。",
                            names.join("、")
                        ));
                    }
                    Err(e) => {
                        code = 1;
                        verdict.push(format!("已删除 {}，但写入测试仍然失败：{e}", names.join("、")));
                    }
                }
            }
        }
    } else if code == 0 {
        verdict.push("存储自检通过：SQLite 可写。".to_string());
    } else {
        verdict.push(
            "存储自检未通过：请在容器内以应用用户重跑 `reng doctor --fix`；\
             若配置目录或 review.db 本身属主不对，需要 root 或宿主机修复。"
                .to_string(),
        );
    }

    let failed = !diagnosis.healthy();
    for line in &lines {
        // Quiet: the green lines of a healthy boot are noise; the failing ones
        // are the entire point.
        if quiet && !line.contains("[FAIL]") {
            continue;
        }
        println!("{line}");
    }
    if !quiet || failed || repaired {
        for line in &verdict {
            println!("{line}");
        }
    }
    code
}

/// Delete the sidecars and re-run the write test — the repair step, kept
/// separate so the decision ([`plan_fix`]) stays pure.
pub async fn delete_sidecars(paths: &[PathBuf], db: &Path) -> Result<(), String> {
    for path in paths {
        // Deleting a file only needs write permission on its DIRECTORY — which
        // is exactly why this repair works without chown and without root.
        // Already gone is success: the goal is that the sidecar is not there,
        // and the write test below is what decides the outcome (SQLite may
        // itself have replaced a sidecar while the diagnosis probed).
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("删除 {} 失败：{e}", path.display())),
        }
    }
    write_test(db).await
}

/// The definitive "can this database be written" verdict: a REAL, harmless WAL
/// write.
///
/// `PRAGMA user_version = <its current value>` reads the cookie and writes it
/// back unchanged: SQLite must take the write lock and dirty page 1, so a
/// database whose sidecars cannot be written answers with its own error
/// (`attempt to write a readonly database` — the incident's message) while no
/// byte of meaning changes. The sqlite message is returned verbatim; the sqlx
/// wrapper around it is not.
pub async fn write_test(db: &Path) -> Result<(), String> {
    let pool = open_read_write(db).await?;
    let result = write_test_pool(&pool).await;
    pool.close().await;
    result
}

/// The same content-preserving write through a pool the caller already holds —
/// the server's own store connection, i.e. the incident exactly as the
/// database's real user experiences it. Kept here so the CLI doctor and the
/// System-Health probe can never drift apart.
pub async fn write_test_pool(pool: &sqlx::AnyPool) -> Result<(), String> {
    rewrite_user_version(pool).await
}

/// The SQLite file the store is backed by, resolved the way the store resolves
/// it: `DATABASE_URL` when that names a SQLite file, otherwise
/// `<state-dir>/review.db`. `None` for PostgreSQL, an in-memory URL, or a
/// process with no state dir at all.
pub fn sqlite_db_path() -> Option<PathBuf> {
    match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => sqlite_path_from_url(&url),
        _ => crate::paths::state_file(DB_FILE_NAME),
    }
}

/// The file behind a `sqlite:` URL; `None` for `sqlite::memory:` and for any
/// other scheme.
pub fn sqlite_path_from_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("sqlite://").or_else(|| url.strip_prefix("sqlite:"))?;
    let path = rest.split('?').next().unwrap_or(rest);
    if path.is_empty() || path.contains(":memory:") {
        return None;
    }
    Some(PathBuf::from(path))
}

async fn open_read_write(db: &Path) -> Result<sqlx::AnyPool, String> {
    ::sqlx::any::install_default_drivers();
    // `mode=rw`, never `rwc`: a doctor must not conjure a missing database —
    // the diagnosis reports that as its own check.
    let url = format!("sqlite://{}?mode=rw", db.display());
    ::sqlx::any::AnyPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .map_err(|e| sqlite_message(&e))
}

async fn rewrite_user_version(pool: &sqlx::AnyPool) -> Result<(), String> {
    let current: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(pool)
        .await
        .map_err(|e| sqlite_message(&e))?;
    sqlx::query(&format!("PRAGMA user_version = {current}"))
        .execute(pool)
        .await
        .map_err(|e| sqlite_message(&e))?;
    Ok(())
}

/// sqlite's own words, without sqlx's `error returned from database: …`
/// wrapper — the report is read by an operator comparing it with the log line
/// the review path produced.
fn sqlite_message(error: &sqlx::Error) -> String {
    match error {
        sqlx::Error::Database(db) => db.message().to_string(),
        other => other.to_string(),
    }
}

/// The exact cause for a file this process cannot write — the string the
/// incident needed, naming both uids when they differ.
pub fn write_cause(state: &FileState) -> String {
    let name = state.name();
    match (state.uid, current_uid()) {
        (Some(owner), Some(mine)) if owner != mine => {
            format!("{name} 属主 uid {owner}，当前进程 uid {mine} 无法写入")
        }
        (Some(owner), Some(_)) => format!(
            "{name} 属主 uid {owner}（当前进程），mode {} 不允许写入",
            state
                .mode
                .map(|m| format!("{m:04o}"))
                .unwrap_or_else(|| "?".to_string())
        ),
        (Some(owner), None) => format!("{name} 属主 uid {owner}，当前进程无法写入"),
        (None, _) => format!("{name} 当前进程无法写入（权限不足）"),
    }
}

/// The sidecar (or `review.db`) ownership/mode cause for `db`, when one of
/// them explains a failed write test — the string the System-Health 存储 row
/// carries so the UI says *why*, not just that writes fail.
pub fn unwritable_cause(db: &Path) -> Option<String> {
    let states: Vec<FileState> = SIDECAR_SUFFIXES
        .iter()
        .map(|suffix| FileState::read(&sidecar_path(&db, suffix)))
        .collect();
    let db_state = FileState::read(db);
    if !db_state.exists {
        return None;
    }
    // The database itself first: it is the file whose owner is the most common
    // cause (a whole config dir copied with `cp -r` as root).
    if !db_state.writable {
        return Some(write_cause(&db_state));
    }
    states
        .iter()
        .filter(|state| state.exists && !state.writable)
        .map(write_cause)
        .next()
}

/// The uid this process effectively runs as.
///
/// No libc dependency: a file the process creates is owned by its effective
/// uid, so one throwaway file in the temp dir answers the question. Cached —
/// an euid never changes within a process, and the diagnosis asks on every
/// failing row.
pub fn current_uid() -> Option<u32> {
    static OWN_UID: OnceLock<Option<u32>> = OnceLock::new();
    *OWN_UID.get_or_init(|| {
        let probe = std::env::temp_dir().join(format!(".reng-doctor-uid-{}", std::process::id()));
        let uid = fs::File::create(&probe)
            .ok()
            .and_then(|file| {
                drop(file);
                fs::metadata(&probe).ok()
            })
            .and_then(|meta| ownership_of(&meta).0);
        let _ = fs::remove_file(&probe);
        uid
    })
}

/// `<db>-wal` / `<db>-shm`.
pub fn sidecar_path(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Create and remove a probe file in `dir`: the only honest answer to "is this
/// directory writable by this process".
fn probe_dir(dir: &Path) -> bool {
    let probe = dir.join(format!(".reng-doctor-{}.tmp", std::process::id()));
    match fs::File::create(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(unix)]
fn ownership_of(meta: &fs::Metadata) -> (Option<u32>, Option<u32>, Option<u32>) {
    use std::os::unix::fs::MetadataExt;
    (Some(meta.uid()), Some(meta.gid()), Some(meta.mode() & 0o7777))
}

#[cfg(not(unix))]
fn ownership_of(_meta: &fs::Metadata) -> (Option<u32>, Option<u32>, Option<u32>) {
    (None, None, None)
}

/// The open-for-write syscall, which is the authority on writability (see
/// [`FileState::writable`]). Opening without writing changes no content; a
/// database file SQLite has open is unaffected.
fn writable_by_this_process(path: &Path) -> bool {
    fs::OpenOptions::new().write(true).open(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A diagnosis with everything the fix decisions read, and nothing else.
    fn diagnosis(write_error: Option<&str>, dir_writable: bool, sidecars: Vec<(&str, u64, bool)>) -> Diagnosis {
        let dir = PathBuf::from("/data/config");
        Diagnosis {
            dir: dir.clone(),
            dir_writable,
            checks: Vec::new(),
            db: FileState {
                path: dir.join(DB_FILE_NAME),
                exists: true,
                uid: Some(9001),
                gid: Some(9001),
                mode: Some(0o644),
                size: 4096,
                writable: true,
            },
            sidecars: sidecars
                .into_iter()
                .map(|(name, size, writable)| FileState {
                    path: dir.join(name),
                    exists: true,
                    uid: Some(1026),
                    gid: Some(1026),
                    mode: Some(0o444),
                    size,
                    writable,
                })
                .collect(),
            write_test: write_error.map(|message| Err(message.to_string())),
        }
    }

    const READONLY: &str = "attempt to write a readonly database";

    /// The repair the incident needed: the write test failed and the sidecars
    /// are the reason, so both go.
    #[test]
    fn plan_deletes_the_sidecars_when_they_are_the_blocker() {
        let diag = diagnosis(
            Some(READONLY),
            true,
            vec![("review.db-wal", 0, false), ("review.db-shm", 0, false)],
        );
        assert_eq!(
            plan_fix(&diag),
            FixPlan::DeleteSidecars(vec![
                PathBuf::from("/data/config/review.db-wal"),
                PathBuf::from("/data/config/review.db-shm"),
            ])
        );
    }

    /// A missing `-wal` is repaired the same way (a `-shm` left behind by a
    /// crashed process is the only sidecar).
    #[test]
    fn plan_deletes_a_lone_unwritable_shm() {
        let diag = diagnosis(Some(READONLY), true, vec![("review.db-shm", 0, false)]);
        assert_eq!(
            plan_fix(&diag),
            FixPlan::DeleteSidecars(vec![PathBuf::from("/data/config/review.db-shm")])
        );
    }

    /// Safety gate: a `-wal` with content may hold committed transactions that
    /// were never checkpointed, so nothing is deleted.
    #[test]
    fn plan_refuses_a_non_empty_wal() {
        let diag = diagnosis(
            Some(READONLY),
            true,
            vec![("review.db-wal", 32768, false), ("review.db-shm", 32768, false)],
        );
        assert_eq!(
            plan_fix(&diag),
            FixPlan::RefuseNonEmptyWal {
                path: PathBuf::from("/data/config/review.db-wal"),
                size: 32768,
            }
        );
    }

    /// The gate is about the `-wal`, not about which sidecar is blocked: a
    /// non-empty, writable `-wal` beside a blocked `-shm` is still refused.
    #[test]
    fn plan_refuses_a_non_empty_wal_even_when_the_wal_itself_is_writable() {
        let diag = diagnosis(
            Some(READONLY),
            true,
            vec![("review.db-wal", 64, true), ("review.db-shm", 0, false)],
        );
        assert!(matches!(plan_fix(&diag), FixPlan::RefuseNonEmptyWal { size: 64, .. }));
    }

    /// An unwritable config dir means deletion is impossible: escalate instead
    /// of failing halfway.
    #[test]
    fn plan_escalates_when_the_config_dir_is_not_writable() {
        let diag = diagnosis(Some(READONLY), false, vec![("review.db-wal", 0, false)]);
        assert_eq!(
            plan_fix(&diag),
            FixPlan::NeedsPrivileges {
                dir: PathBuf::from("/data/config")
            }
        );
    }

    /// `review.db` itself unwritable: deleting sidecars cannot help, and the
    /// file must keep its data — this one is root's job.
    #[test]
    fn plan_leaves_an_unwritable_database_to_root() {
        let diag = diagnosis(Some(READONLY), true, vec![]);
        assert_eq!(plan_fix(&diag), FixPlan::Nothing);
    }

    /// A failing write test with writable sidecars has another cause: nothing
    /// is mutated on a guess.
    #[test]
    fn plan_does_not_mutate_when_the_sidecars_are_writable() {
        let diag = diagnosis(Some(READONLY), true, vec![("review.db-wal", 0, true)]);
        assert_eq!(plan_fix(&diag), FixPlan::Nothing);
    }

    /// Healthy storage is never touched, whatever the sidecars' owner says.
    #[test]
    fn plan_is_empty_when_the_write_test_passes() {
        let mut diag = diagnosis(None, true, vec![("review.db-wal", 4096, false)]);
        diag.write_test = Some(Ok(()));
        assert_eq!(plan_fix(&diag), FixPlan::Nothing);
    }

    /// The cause string the incident needed: both uids, and the file's name.
    #[test]
    fn write_cause_names_both_uids_on_an_ownership_mismatch() {
        let own = current_uid().expect("tests run as a uid the filesystem reports");
        let state = FileState {
            path: PathBuf::from("/data/config/review.db-wal"),
            exists: true,
            uid: Some(if own == 0 { 1 } else { own + 1 }),
            gid: Some(0),
            mode: Some(0o644),
            size: 0,
            writable: false,
        };
        let cause = write_cause(&state);
        assert!(cause.starts_with("review.db-wal 属主 uid "), "{cause}");
        assert!(cause.contains(&format!("当前进程 uid {own}")), "{cause}");
        assert!(cause.ends_with("无法写入"), "{cause}");
    }

    // ─── against a real SQLite file, rootless ───

    fn chmod(path: &Path, mode: u32) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = (path, mode);
    }

    /// A real WAL-mode database with one row, created the way the shipped
    /// deployment creates it. A clean close checkpoints and removes the
    /// sidecars, exactly the state a healthy install is in.
    async fn seed_wal_db(path: &Path) {
        ::sqlx::any::install_default_drivers();
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = ::sqlx::any::AnyPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("seed database");
        sqlx::query("PRAGMA journal_mode = WAL").fetch_all(&pool).await.unwrap();
        sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO t (v) VALUES ('x')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    /// A permission-based test needs a non-root uid: root bypasses every mode
    /// bit, so `chmod 0444` would not stop it.
    fn rootless() -> bool {
        current_uid() != Some(0)
    }

    /// The incident, end to end and rootless: sidecars this process cannot
    /// write → the checks report it → `--fix` deletes them → a real write
    /// succeeds again.
    #[tokio::test]
    async fn fix_removes_unwritable_sidecars_and_the_database_becomes_writable() {
        if !rootless() {
            eprintln!("skipped: root bypasses the mode bits this test relies on");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join(DB_FILE_NAME);
        seed_wal_db(&db).await;
        for suffix in SIDECAR_SUFFIXES {
            let path = sidecar_path(&db, suffix);
            fs::write(&path, b"").unwrap();
            chmod(&path, 0o444);
        }
        let diag = inspect(tmp.path()).await;
        assert!(diag.dir_writable, "the temp dir is writable");
        assert!(diag.db.writable, "review.db itself stays writable: {:?}", diag.db);
        assert!(!diag.healthy(), "unwritable sidecars must fail the report");
        let check = diag.check("review.db-shm").expect("the -shm is reported");
        assert!(!check.ok && !check.skipped, "{check:?}");
        let error = diag
            .write_test
            .clone()
            .expect("a database exists, so the write test runs")
            .expect_err("a database whose sidecars cannot be written is not writable");
        assert!(
            !error.contains("error returned from database"),
            "the report carries sqlite's own words, not sqlx's wrapper: {error}"
        );

        let plan = plan_fix(&diag);
        let FixPlan::DeleteSidecars(paths) = plan.clone() else {
            panic!("the sidecars must be the repair, got {plan:?}");
        };
        assert_eq!(paths.len(), 2, "both sidecars go: {paths:?}");
        delete_sidecars(&paths, &db).await.expect("the repair succeeds");
        for path in &paths {
            assert!(!path.exists(), "{} must be gone", path.display());
        }
        // And the database is genuinely writable again — the post-fix verdict
        // is a real write, not an assumption.
        assert!(
            write_test(&db).await.is_ok(),
            "the database must be writable after the sidecars are removed"
        );
        let after = inspect(tmp.path()).await;
        assert!(after.healthy(), "the repaired store passes every check: {after:?}");
        assert_eq!(plan_fix(&after), FixPlan::Nothing);
    }

    /// A non-empty `-wal` is refused, and nothing is deleted.
    #[tokio::test]
    async fn a_non_empty_wal_is_refused_and_left_alone() {
        if !rootless() {
            eprintln!("skipped: root bypasses the mode bits this test relies on");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join(DB_FILE_NAME);
        seed_wal_db(&db).await;
        let wal = sidecar_path(&db, "-wal");
        fs::write(&wal, vec![7u8; 4096]).unwrap();
        chmod(&wal, 0o444);

        let diag = inspect(tmp.path()).await;
        assert!(matches!(plan_fix(&diag), FixPlan::RefuseNonEmptyWal { size: 4096, .. }));
        assert!(wal.exists(), "a refusal never deletes anything");
        assert_eq!(fs::metadata(&wal).unwrap().len(), 4096);
    }

    /// The PASS path: everything writable → every check passes, the write test
    /// is a real write, and no data changes.
    #[tokio::test]
    async fn a_healthy_store_passes_every_check_without_mutating_anything() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join(DB_FILE_NAME);
        seed_wal_db(&db).await;
        let before = change_counter(&fs::read(&db).unwrap());

        let diag = inspect(tmp.path()).await;
        assert!(diag.healthy(), "healthy storage must pass: {diag:?}");
        assert!(matches!(diag.write_test, Some(Ok(()))), "{diag:?}");
        assert_eq!(plan_fix(&diag), FixPlan::Nothing);
        // The probe is a REAL write — sqlite's header change counter advances
        // (which is also why `PRAGMA user_version = <same>` detects a readonly
        // database instead of silently doing nothing) — while the database's
        // meaning is untouched: same user_version, same rows.
        assert!(
            change_counter(&fs::read(&db).unwrap()) > before,
            "the write test must be a write"
        );
        let pool = open_read_write(&db).await.unwrap();
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&pool)
            .await
            .unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM t")
            .fetch_one(&pool)
            .await
            .unwrap();
        pool.close().await;
        assert_eq!((version, rows), (0, 1), "user_version and the data are unchanged");
        // Only the files the store owns: no probe file left behind.
        let leftovers: Vec<String> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".reng-doctor"))
            .collect();
        assert!(leftovers.is_empty(), "probe files must be removed: {leftovers:?}");
    }

    /// A sidecar SQLite itself removed between the diagnosis and the repair
    /// (its own probe may replace one) must not fail the repair — the goal is
    /// that the file is not there, and the re-run write test decides.
    #[tokio::test]
    async fn delete_sidecars_tolerates_one_that_is_already_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join(DB_FILE_NAME);
        seed_wal_db(&db).await;
        let missing = sidecar_path(&db, "-wal");
        assert!(!missing.exists());
        delete_sidecars(std::slice::from_ref(&missing), &db)
            .await
            .expect("an absent sidecar is not a repair failure");
        assert!(write_test(&db).await.is_ok());
    }

    /// The database header's file-change counter (offset 24, big endian): sqlite
    /// bumps it on every committed write transaction.
    fn change_counter(bytes: &[u8]) -> u32 {
        u32::from_be_bytes([bytes[24], bytes[25], bytes[26], bytes[27]])
    }

    /// A missing database is reported — and the write test is skipped rather
    /// than blamed, because `mode=rw` would fail on a file that does not
    /// exist yet.
    #[tokio::test]
    async fn a_missing_database_fails_its_check_and_skips_the_write_test() {
        let tmp = tempfile::tempdir().unwrap();
        let diag = inspect(tmp.path()).await;
        assert!(!diag.healthy());
        let check = diag.check("review.db").expect("the database is reported");
        assert!(!check.ok && !check.skipped, "{check:?}");
        assert!(check.detail.contains("不存在"), "{check:?}");
        let write = diag.check("write-test").expect("the write test is reported");
        assert!(write.skipped && !write.ok, "{write:?}");
        assert_eq!(diag.write_test, None);
        // Nothing to repair, and nothing invented.
        assert_eq!(plan_fix(&diag), FixPlan::Nothing);
        assert!(!tmp.path().join(DB_FILE_NAME).exists());
    }
}
