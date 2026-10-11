//! Secure file I/O for the `--verbose` raw-LLM dump path.
//!
//! RENG-113 / M18-W1: the report markdown and the publish path already use
//! [`crate::cli::handlers::output::write_report_file`], which creates the
//! report file with `OpenOptions::mode(0o600)` and redacts
//! credential-shaped content. The `--verbose` dump directory (`<output>.raw/`
//! or `<output_dir>/review-raw/`) was still being created with the user's
//! umask and the prompt / response files inside it were being written with
//! `std::fs::write` — so the directory landed at `0775` and the files at
//! `0664`, leaving the raw LLM exchange (which can quote a credential-shaped
//! fixture exactly the way the markdown report does) readable by every
//! other account on the host. This module is the dump-side counterpart of
//! RENG-113: directory at `0700`, files at `0600`, response text passed
//! through [`crate::models::secrets::redact_text`] before it hits disk.
//!
//! On non-Unix targets both helpers still create the directory and write
//! the file — the only thing skipped is the `mode(0o600)` /
//! `set_permissions(0o600)` / `set_permissions(0o700)` enforcement, since
//! `OpenOptions::mode` is a Unix-only `OpenOptionsExt` method. A permissive
//! Windows ACL is a separate problem and is left for the non-Unix
//! `write_report_file` fallback it mirrors.

use std::path::{Path, PathBuf};

/// Create the verbose-dump directory with owner-only (`0700`) permissions.
///
/// Returns the directory on success and `None` on failure, matching the
/// pre-existing warning-and-fallback contract used by the CLI handler: a
/// directory that can't be created must not abort the run, but the
/// warning must be visible on stderr so the operator can see what was
/// dropped. Returns the path unchanged on non-Unix targets.
pub fn create_secure_dump_dir(dir: &Path) -> Option<PathBuf> {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("warning: [verbose] failed to create dump dir {}: {e}", dir.display());
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // create_dir_all honours the umask; chmod back to 0700 so the raw
        // LLM exchange is owner-only even when the user's umask is 022.
        if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
            eprintln!(
                "warning: [verbose] failed to chmod 0o700 dump dir {}: {e}",
                dir.display()
            );
        }
    }
    Some(dir.to_path_buf())
}

/// Persist `text` as a verbose-dump artifact with owner-only (`0600`)
/// permissions, after passing it through
/// [`crate::models::secrets::redact_text`]. Failures are reported on stderr
/// without aborting the run, matching the dump contract: an unwritable
/// artifact must not silence the rest of the review.
///
/// The `redact_text` pass is the same one used by the report markdown
/// (RENG-113): credential-shaped values are replaced with
/// `***[redacted:kind=…,key=…,len=…,fp=…]***` markers that preserve the
/// value's length and fingerprint so a reviewer can still tell "two
/// findings cite the same value" without seeing the value itself.
pub fn write_secure_dump_file(path: &Path, text: &str) -> std::io::Result<()> {
    let body = crate::models::redact_text(text);
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        // OpenOptions::mode(0o600) sets the inode mode before the file is
        // reachable on the filesystem, so there is no window where another
        // process can open() the dump under a permissive umask-derived
        // mode. The chmod belt-and-braces stays for the overwrite path
        // (truncate does not change mode on an existing file).
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(body.as_bytes())?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, body.as_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Per-process monotonic counter — each test in this module produces a
    /// fresh subdirectory under `std::env::temp_dir()`, and reusing a name
    /// across runs would re-use the previous run's permissions.
    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn fresh_dump_dir(tag: &str) -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("reng-verbose-dump-{tag}-{pid}-{seq}"));
        dir
    }

    /// RENG-113 + M18-W1: the dump directory must land at `0700` so the raw
    /// LLM exchange is owner-only even when the user has a permissive
    /// umask. We do not assert a tighter mode (the chmod path could be
    /// bypassed by an `setuid` binary the operator is running under) — the
    /// contract is "not world- or group-readable", which `0700` enforces.
    #[cfg(unix)]
    #[test]
    fn create_secure_dump_dir_lands_at_0700() {
        use std::os::unix::fs::PermissionsExt;
        let dir = fresh_dump_dir("dir-mode");
        // Force a permissive umask-derived mode first so the chmod path is
        // the thing being tested, not the inherited mode of the dir.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let created = create_secure_dump_dir(&dir).expect("create_dir_all must succeed");
        assert_eq!(created, dir);

        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "dump dir must be owner-only; got {mode:o}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The dump file the LLM response lands in must be at `0600`. The same
    /// reasoning as the directory test: a permissive umask-derived `0644`
    /// would expose the response (which can quote a credential-shaped
    /// fixture) to every other account on the host.
    #[cfg(unix)]
    #[test]
    fn write_secure_dump_file_lands_at_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = fresh_dump_dir("file-mode");
        create_secure_dump_dir(&dir).unwrap();
        let path = dir.join("response.txt");
        // Pre-create the file at a permissive mode so we exercise the
        // belt-and-braces chmod path, not just OpenOptions::mode.
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_secure_dump_file(&path, "new body").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "dump file must be owner-only; got {mode:o}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new body");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The response written through this path must have credential-shaped
    /// values replaced by `***[redacted:kind=…,key=…,len=…,fp=…]***`
    /// markers, identical to the report markdown's pass — that is the
    /// whole point of routing the LLM response through `redact_text`
    /// before it hits disk.
    #[test]
    fn write_secure_dump_file_redacts_credential_shaped_values() {
        let dir = fresh_dump_dir("redact");
        create_secure_dump_dir(&dir).unwrap();
        let path = dir.join("response.txt");

        let body = "Found a credential: password = \"hunter2_real\"\nrest is fine\n";
        write_secure_dump_file(&path, body).unwrap();

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            !on_disk.contains("hunter2_real"),
            "plaintext credential must not survive the dump write: {on_disk}"
        );
        assert!(on_disk.contains("password = "), "the key survives: {on_disk}");
        assert!(on_disk.contains("kind=password"), "the kind is named: {on_disk}");
        assert!(
            on_disk.contains("rest is fine"),
            "unrelated lines are untouched: {on_disk}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// When the directory cannot be created, the helper must return None
    /// and never panic — the CLI handler depends on this contract to keep
    /// the review running on a permission failure.
    #[cfg(unix)]
    #[test]
    fn create_secure_dump_dir_returns_none_on_failure() {
        // A path that already points at a regular file can't become a
        // directory, so create_dir_all fails. We don't care which error
        // surfaces — only that the helper degrades to None.
        let dir = fresh_dump_dir("fail");
        std::fs::write(&dir, "not a dir").unwrap();
        assert!(create_secure_dump_dir(&dir).is_none());
        let _ = std::fs::remove_file(&dir);
    }
}
