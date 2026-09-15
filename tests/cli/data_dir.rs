use super::*;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// ─────────────────────────────────────────────────────────────────────
// RENG-37: `serve --data-dir <path>` — every persisted artifact lands
// under that path, two instances share nothing, and the default
// `~/.config/review-engine` is never created or touched.
// ─────────────────────────────────────────────────────────────────────

/// Per-artifact overrides are process-wide and deliberately win over the data
/// dir (documented in `review_engine::paths`); clear them so the test measures
/// `--data-dir` alone. `DATABASE_URL` would swap the SQLite file for a server.
const ARTIFACT_ENV_VARS: &[&str] = &[
    "REVIEW_DATA_DIR",
    "REVIEW_ENGINE_CONFIG_DIR",
    "REVIEW_UI_STATE_FILE",
    "REVIEW_AUTH_FILE",
    "REVIEW_DISPATCH_STATE",
    "REVIEW_FEEDBACK_PATH",
    "REVIEW_MODELS_DEV_CACHE",
    "DATABASE_URL",
    "REVIEW_DISABLE_DB",
];

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("failed to bind ephemeral port");
    listener.local_addr().expect("no local addr").port()
}

/// A `serve --data-dir <dir>` process, killed by [`Instance::stop`].
struct Instance {
    data_dir: PathBuf,
    child: Option<std::process::Child>,
    /// Everything the child wrote to the pipe so far; the readers below keep
    /// appending while it runs, so a kill can never cut off a flushed line.
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    readers: Option<Vec<std::thread::JoinHandle<()>>>,
}

/// Drain one of the child's pipes into `sink` on its own thread.
///
/// Reading has to start as soon as the pipe exists and continue until EOF: the
/// process is killed rather than asked to stop, so the only place its output
/// survives is this buffer, and an unread pipe would eventually fill and block
/// the child. `read_until` splits on newlines but keeps the bytes as written
/// (a partial line is completed by the next read), so `sink` ends up holding
/// exactly what the child flushed.
fn drain(pipe: impl std::io::Read + Send + 'static, sink: &Arc<Mutex<Vec<u8>>>) -> std::thread::JoinHandle<()> {
    let sink = Arc::clone(sink);
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(pipe);
        let mut line = Vec::new();
        while reader.read_until(b'\n', &mut line).unwrap_or(0) > 0 {
            sink.lock().unwrap().extend_from_slice(&line);
            line.clear();
        }
    })
}

/// The startup banner `serve` prints once its listener is bound; it ends with
/// the resolved `logs.ndjson` path, which is what the callers assert on.
const BANNER_PREFIX: &str = "review-engine listening on ";

/// Whether the *complete* banner line is in `stdout` yet.
///
/// Only newline-terminated text counts: a half-read line would still carry a
/// truncated log path, so declaring it arrived early would just move the race
/// into the assertions.
fn banner_is_complete(stdout: &[u8]) -> bool {
    stdout
        .split_inclusive(|byte| *byte == b'\n')
        .any(|line| line.ends_with(b"\n") && String::from_utf8_lossy(line).starts_with(BANNER_PREFIX))
}

impl Instance {
    fn start(data_dir: &Path, home: &Path) -> Self {
        let mut cmd = Command::new(bin_path());
        cmd.args([
            "serve",
            "--port",
            &free_port().to_string(),
            "--data-dir",
            &data_dir.display().to_string(),
        ])
        .env("HOME", home)
        .current_dir(home);
        for key in ARTIFACT_ENV_VARS {
            cmd.env_remove(key);
        }
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().expect("failed to spawn review-engine serve");

        let stdout = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let readers = vec![
            drain(child.stdout.take().expect("stdout is piped"), &stdout),
            drain(child.stderr.take().expect("stderr is piped"), &stderr),
        ];

        Self {
            data_dir: data_dir.to_path_buf(),
            child: Some(child),
            stdout,
            stderr,
            readers: Some(readers),
        }
    }

    fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout.lock().unwrap()).into_owned()
    }

    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr.lock().unwrap()).into_owned()
    }

    /// Join the pipe readers. The child is gone by then, so both hit EOF and
    /// the buffers are final.
    fn join_readers(&mut self) {
        for reader in self.readers.take().unwrap_or_default() {
            let _ = reader.join();
        }
    }

    /// Wait for the first startup write (`review.db`) — reaching it means the
    /// database, `secrets.key` and `logs.ndjson` have all been resolved.
    fn wait_for_db(&mut self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if self.data_dir.join("review.db").exists() {
                return;
            }
            if let Some(status) = self.child.as_mut().and_then(|child| child.try_wait().ok().flatten()) {
                self.child = None;
                self.join_readers();
                panic!(
                    "serve exited early ({status}) without writing {}/review.db\nstderr: {}",
                    self.data_dir.display(),
                    self.stderr_text()
                );
            }
            assert!(
                std::time::Instant::now() < deadline,
                "serve never wrote {}/review.db within 30s",
                self.data_dir.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Block until the complete startup banner has reached stdout, the child
    /// exits, or 30s pass. Returns whether the banner arrived.
    fn wait_for_banner(&mut self) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if banner_is_complete(&self.stdout.lock().unwrap()) {
                return true;
            }
            let exited = self
                .child
                .as_mut()
                .and_then(|child| child.try_wait().ok().flatten())
                .is_some();
            if exited || std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    /// Kill the child and return what it wrote to stdout.
    ///
    /// `wait_for_db` only proves the database file exists; the banner naming
    /// the log file is printed later, once the listener is bound. Killing
    /// straight after the database appears raced that banner — under CI load
    /// it was still unread in the pipe, so callers asserted against empty
    /// stdout (RENG-74). Waiting for the banner here is safe to combine with
    /// the kill because the readers have been draining the pipe since `start`,
    /// i.e. every flushed line lands in the buffer either way.
    fn stop(mut self) -> String {
        let banner = self.wait_for_banner();
        let mut child = self.child.take().expect("child");
        if child.try_wait().expect("poll review-engine serve").is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
        self.join_readers();
        assert!(
            banner,
            "serve never printed its startup banner within 30s\nstdout: {}\nstderr: {}",
            self.stdout_text(),
            self.stderr_text()
        );
        self.stdout_text()
    }
}

/// (d) Two instances, two data dirs, one `HOME`: each gets its own database,
/// secrets key and log file, and the default config dir stays untouched.
#[test]
fn two_instances_with_different_data_dirs_share_no_state() {
    let home = TempDir::new().unwrap();
    let data_a = home.path().join("state-a");
    let data_b = home.path().join("state-b");
    let default_config_dir = home.path().join(".config").join("review-engine");
    assert!(!data_a.exists(), "--data-dir must create the directory itself");
    assert!(!data_b.exists());

    let mut a = Instance::start(&data_a, home.path());
    a.wait_for_db();
    let stdout_a = {
        // Snapshot what A wrote before B starts.
        for artifact in ["review.db", "secrets.key", "logs.ndjson"] {
            assert!(
                data_a.join(artifact).exists(),
                "instance A must write {artifact} under its data dir"
            );
        }
        assert!(
            !default_config_dir.exists(),
            "instance A must not create the default config dir"
        );
        a.stop()
    };
    assert!(
        stdout_a.contains(&data_a.join("logs.ndjson").display().to_string()),
        "the startup banner must name the data dir's log file, got: {stdout_a}"
    );
    assert!(
        !stdout_a.contains(&home.path().join(".config").display().to_string()),
        "the banner must not point into the default config dir, got: {stdout_a}"
    );

    let mut b = Instance::start(&data_b, home.path());
    b.wait_for_db();
    let stdout_b = b.stop();

    // Each instance owns its own database and key file: distinct paths, both
    // present, and B's database was created independently of A's.
    let db_a = data_a.join("review.db");
    let db_b = data_b.join("review.db");
    assert!(
        db_a.is_file() && db_b.is_file(),
        "both instances need their own review.db"
    );
    assert_ne!(db_a, db_b);
    assert!(
        data_b.join("secrets.key").is_file(),
        "instance B must create its own secrets.key"
    );
    assert_ne!(
        std::fs::read(data_a.join("secrets.key")).unwrap(),
        std::fs::read(data_b.join("secrets.key")).unwrap(),
        "the at-rest keys must not be shared between instances"
    );
    assert!(
        stdout_b.contains(&data_b.join("logs.ndjson").display().to_string()),
        "instance B must log under its own data dir, got: {stdout_b}"
    );
    assert!(
        !default_config_dir.exists(),
        "neither instance may create the default config dir"
    );
}

/// The default stays byte-for-byte what it was: without `--data-dir` the
/// state lands in `$HOME/.config/review-engine`, not in some new location.
#[test]
fn without_the_flag_state_still_lands_in_the_default_config_dir() {
    let home = TempDir::new().unwrap();
    let mut cmd = Command::new(bin_path());
    cmd.args(["serve", "--port", &free_port().to_string()])
        .env("HOME", home.path())
        .current_dir(home.path());
    for key in ARTIFACT_ENV_VARS {
        cmd.env_remove(key);
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("failed to spawn review-engine serve");

    let expected = home.path().join(".config").join("review-engine");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if expected.join("review.db").exists() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "serve never wrote {}/review.db within 30s",
            expected.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait_with_output();

    for artifact in ["review.db", "secrets.key", "logs.ndjson"] {
        assert!(
            expected.join(artifact).is_file(),
            "without --data-dir {artifact} must stay in {}",
            expected.display()
        );
    }
}
