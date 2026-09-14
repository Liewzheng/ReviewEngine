use super::*;
use std::path::PathBuf;

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
        Self {
            data_dir: data_dir.to_path_buf(),
            child: Some(cmd.spawn().expect("failed to spawn review-engine serve")),
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
            if let Some(child) = self.child.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    let output = self
                        .child
                        .take()
                        .expect("child")
                        .wait_with_output()
                        .expect("collect output");
                    panic!(
                        "serve exited early ({status}) without writing {}/review.db\nstderr: {}",
                        self.data_dir.display(),
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "serve never wrote {}/review.db within 30s",
                self.data_dir.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn stop(mut self) -> String {
        let mut child = self.child.take().expect("child");
        let _ = child.kill();
        let output = child.wait_with_output().expect("failed to collect output");
        String::from_utf8_lossy(&output.stdout).into_owned()
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
