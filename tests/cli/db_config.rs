//! `--config-dir` and database-over-file priority on the command line
//! (RENG-107).
//!
//! Three things are verified against the REAL binary, because none of them can
//! be seen from a unit test:
//!
//! - `--config-dir <path>` is the global flag form of the state root: it is
//!   accepted by commands other than `serve`, it decides where the deployment
//!   configuration is read from, and it sits in the documented precedence;
//! - a `review.db` in that directory is what a manual `reng review` runs on —
//!   this is the user's own scenario ("让手动 reng review 吃到 WebUI 在 DB 里配的
//!   真实 LLM 密钥"), verified by pointing the database's provider at a mock
//!   HTTP server and asserting the request carried the database's key;
//! - the CLI never writes that database: no `-wal` / `-shm` appears and the
//!   `review.db` bytes and mtime are untouched.
//!
//! Every process is spawned with `$HOME` pointed at a temp dir and the
//! steering variables (`REVIEW_DATA_DIR`, `REVIEW_ENGINE_CONFIG_DIR`,
//! `DATABASE_URL`, `LLM_CONFIG`, the per-artifact paths, …) removed, so the
//! assertions measure the flag and the database rather than the machine the
//! tests run on.

use super::*;
use review_engine::models::LLMConfig;
use review_engine::server::api::config::persist::UI_SETTING_KEY;
use review_engine::server::api::config::UiConfig;
use review_engine::store::traits::ConfigStore;
use review_engine::store::SqlxStore;
use std::path::Path;
use std::process::{Command, Stdio};

/// Every variable that can steer the state root or the configuration from
/// outside the test. Cleared for each spawned process.
const STEERING_ENV: &[&str] = &[
    "REVIEW_DATA_DIR",
    "REVIEW_ENGINE_CONFIG_DIR",
    "REVIEW_UI_STATE_FILE",
    "REVIEW_AUTH_FILE",
    "REVIEW_DISPATCH_STATE",
    "REVIEW_FEEDBACK_PATH",
    "REVIEW_MODELS_DEV_CACHE",
    "DATABASE_URL",
    "REVIEW_DISABLE_DB",
    "LLM_CONFIG",
    "GITLAB_TOKEN",
    "GITHUB_TOKEN",
];

fn stdout_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Spawn the binary with an isolated `$HOME`, the steering variables cleared
/// and `extra_env` applied on top.
fn isolated_command(args: &[&str], cwd: &Path, home: &Path, extra_env: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(bin_path());
    cmd.args(args).current_dir(cwd).env("HOME", home);
    for key in STEERING_ENV {
        cmd.env_remove(key);
    }
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd
}

fn run_isolated(args: &[&str], cwd: &Path, home: &Path, extra_env: &[(&str, &str)]) -> std::process::Output {
    isolated_command(args, cwd, home, extra_env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute review-engine")
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("failed to bind ephemeral port");
    listener.local_addr().expect("no local addr").port()
}

/// A valid config file: the built-in expert team (weights sum to 100) plus one
/// provider, which is all a `validate` or a review needs.
fn config_toml(llm: Option<(&str, &str, &str)>) -> String {
    match llm {
        Some((provider, api_base, api_key)) => format!(
            "[[llm]]\nprovider = \"{provider}\"\nmodel = \"toml-mock\"\napi_base = \"{api_base}\"\napi_key = \"{api_key}\"\n"
        ),
        None => String::new(),
    }
}

/// Make `dir` the state root the CLI is pointed at (`--config-dir`), with an
/// optional deployment config file in it.
fn make_state_root(dir: &Path, config: Option<String>) {
    std::fs::create_dir_all(dir).unwrap();
    if let Some(toml) = config {
        std::fs::write(dir.join(".code-audit-config.toml"), toml).unwrap();
    }
}

/// A git repository with one commit and one UNCOMMITTED change — the shape
/// `review --local-path . --base main` reviews (the diff is the working tree
/// against `main`).
fn repo_with_a_change() -> TempDir {
    let repo = TempDir::new().unwrap();
    let path = repo.path();
    git_init(path);
    git_config_user(path);
    std::fs::create_dir_all(path.join("src")).unwrap();
    std::fs::write(path.join("src/lib.rs"), "pub fn initial() -> u32 {\n    1\n}\n").unwrap();
    git_add_and_commit(path, "initial commit");
    // Uncommitted: the change under review.
    std::fs::write(
        path.join("src/lib.rs"),
        "pub fn initial() -> u32 {\n    1\n}\n\npub fn added() -> u32 {\n    initial() + 1\n}\n",
    )
    .unwrap();
    repo
}

/// One YAML body naming a finding in the reviewed file, so the pipeline keeps
/// it (`validate_findings` drops findings whose file is not in the diff).
fn findings_body(title: &str, api_key_marker: &str) -> String {
    format!(
        "review:\n  findings:\n    - file: \"src/lib.rs\"\n      line: 1\n      severity: \"high\"\n      title: \"{title}\"\n      detail: \"api key seen by the mock: {api_key_marker}\"\n"
    )
}

async fn mount_mock_llm(server: &wiremock::MockServer, body: String) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": body}}],
            "model": "mock-model",
            "usage": {"total_tokens": 1}
        })))
        .mount(server)
        .await;
}

fn db_card(provider: &str, uri: &str, api_key: &str) -> LLMConfig {
    LLMConfig {
        provider: provider.to_string(),
        model: "db-mock".to_string(),
        api_key: api_key.to_string(),
        api_base: format!("{uri}/v1"),
        max_tokens: 4096,
        temperature: 0.2,
        disable_thinking: None,
        disabled: false,
    }
}

/// The `ui` row a WebUI save writes: the provider the user picked as primary
/// plus the advanced concurrency cap.
///
/// The cap is not decoration. A `ui` row whose `advanced` section is ABSENT
/// deserializes to `UiAdvancedConfig::default()`, whose derived `Default` gives
/// `maxConcurrentReviews: 0` — `apply_db_overrides` then maps that onto
/// `max_concurrent_llm_calls`/`max_team_size = Some(0)`, and the review
/// pipeline builds a **0-permit semaphore** (`team::orchestrator::pipeline`),
/// so `reng review` hangs instead of running (found while writing this test;
/// filed as a finding, the fix belongs in `db_overlay`). Every row the Web UI
/// writes carries the section (`UiConfig::from_app_config` seeds it), so the
/// realistic row is the one below.
fn webui_ui_row(primary: &str) -> serde_json::Value {
    serde_json::json!({
        "llm": { "primaryProvider": primary },
        "advanced": { "maxConcurrentReviews": 5 },
    })
}

/// Seed the state root's `review.db` exactly as the server does (same store,
/// same migration, `enc:` secrets under the directory's `secrets.key`), then
/// leave it in the checkpointed shape: no writers, no sidecars. That is the
/// state a CLI must read without creating anything.
async fn seed_state_root_database(dir: &Path, cards: &[LLMConfig], primary: Option<&str>) {
    let store = SqlxStore::connect_default(dir).await.unwrap();
    store.migrate().await.unwrap();
    store.replace_llm_providers(cards).await.unwrap();
    if let Some(primary) = primary {
        let ui: UiConfig = serde_json::from_value(webui_ui_row(primary)).unwrap();
        store
            .save_setting(UI_SETTING_KEY, &serde_json::to_value(&ui).unwrap())
            .await
            .unwrap();
    }
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE);")
        .execute(store.pool())
        .await
        .unwrap();
    store.pool().close().await;
    // The checkpoint moved every committed frame into review.db; drop the
    // (now empty) sidecars so the test starts from the shape the CLI meets
    // after a clean shutdown. The `-shm` is the wal-index and keeps its 32 KB
    // page regardless — only the WAL's emptiness says "no data lives here".
    let db = dir.join(review_engine::paths::DB_FILE_NAME);
    let wal = review_engine::doctor::sidecar_path(&db, "-wal");
    if wal.exists() {
        assert_eq!(
            std::fs::metadata(&wal).unwrap().len(),
            0,
            "{} must be empty after the checkpoint",
            wal.display()
        );
    }
    for suffix in ["-wal", "-shm"] {
        let sidecar = review_engine::doctor::sidecar_path(&db, suffix);
        if sidecar.exists() {
            std::fs::remove_file(&sidecar).unwrap();
        }
    }
}

/// The database's own files: contents, mtime and the sidecar set. Anything
/// that changes here means the CLI wrote to the configuration database.
fn db_fingerprint(dir: &Path) -> (Vec<u8>, std::time::SystemTime, Vec<String>) {
    let db = dir.join(review_engine::paths::DB_FILE_NAME);
    let mut sidecars: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("review.db"))
        .collect();
    sidecars.sort();
    (
        std::fs::read(&db).unwrap(),
        std::fs::metadata(&db).unwrap().modified().unwrap(),
        sidecars,
    )
}

/// The `Authorization` headers a mock server received, in order.
async fn authorization_headers(server: &wiremock::MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|req| {
            req.headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
        .collect()
}

/// The result of [`run_with_deadline`].
struct FinishedRun {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Run the binary with a HARD deadline, capturing its output through files.
///
/// The hang tests exist because `reng` used to wait forever on a 0-permit
/// semaphore; if that ever comes back, a test that simply waits would hang the
/// whole suite instead of failing. Files rather than pipes, because a killed
/// child must not be able to block on a full pipe.
fn run_with_deadline(args: &[&str], cwd: &Path, home: &Path, seconds: u64) -> FinishedRun {
    let out_path = home.join("deadline-stdout.txt");
    let err_path = home.join("deadline-stderr.txt");
    let mut child = isolated_command(args, cwd, home, &[])
        .stdout(std::fs::File::create(&out_path).expect("create stdout file"))
        .stderr(std::fs::File::create(&err_path).expect("create stderr file"))
        .spawn()
        .expect("failed to spawn review-engine");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let mut finished = false;
    while !finished {
        match child.try_wait().expect("try_wait failed") {
            Some(_) => finished = true,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "review-engine did not finish within {seconds}s — the zero-cap hang is back. \
                     stdout: {} stderr: {}",
                    std::fs::read_to_string(&out_path).unwrap_or_default(),
                    std::fs::read_to_string(&err_path).unwrap_or_default()
                );
            }
            None => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    }
    let status = child.wait().expect("wait failed");
    FinishedRun {
        success: status.success(),
        stdout: std::fs::read_to_string(&out_path).unwrap_or_default(),
        stderr: std::fs::read_to_string(&err_path).unwrap_or_default(),
    }
}

// ─── `--config-dir` ────────────────────────────────────────────────

/// The flag is global (accepted by a command that has no `--data-dir` of its
/// own) and it is what decides where the deployment configuration is read
/// from — the home default is never consulted.
#[test]
fn config_dir_flag_moves_the_state_root_for_a_non_serve_command() {
    let cwd = TempDir::new().unwrap();
    let root = TempDir::new().unwrap();
    let root = root.path().join("volume1-docker-reng-config");
    make_state_root(&root, Some(config_toml(None)));

    // Control: without the flag, `validate` resolves the user-level config
    // under $HOME, which does not exist here.
    let baseline_home = TempDir::new().unwrap();
    let baseline = run_isolated(&["validate"], cwd.path(), baseline_home.path(), &[]);
    assert!(!baseline.status.success(), "expected no config to be found");
    assert!(
        stderr_of(&baseline).contains("No config file found"),
        "stderr: {}",
        stderr_of(&baseline)
    );

    // A fresh $HOME for the real run: nothing of the control run's can make
    // the assertions below pass by accident.
    let home = TempDir::new().unwrap();
    let output = run_isolated(
        &["--config-dir", root.to_str().unwrap(), "validate"],
        cwd.path(),
        home.path(),
        &[],
    );
    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    assert!(
        stdout_of(&output).contains("Valid config"),
        "stdout: {}",
        stdout_of(&output)
    );
    // Positive evidence that the flag root is the state root: the CLI's own
    // runtime artifacts land there.
    assert!(
        root.join(review_engine::paths::LOG_FILE_NAME).is_file(),
        "the CLI must log into the --config-dir root"
    );
    assert!(
        !home
            .path()
            .join(".config")
            .join("review-engine")
            .join(review_engine::paths::USER_CONFIG_FILE_NAME)
            .exists(),
        "the flag must not fall back to the home default"
    );
}

/// The pinned precedence, for the part a CLI-only run can observe:
/// `--config-dir` > `REVIEW_DATA_DIR` > `REVIEW_ENGINE_CONFIG_DIR`.
///
/// The two roots hold configs that fail and pass validation respectively, so
/// which one was read is unambiguous from the exit code.
#[test]
fn config_dir_flag_outranks_the_data_dir_and_config_dir_env_vars() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let flag_root = home.path().join("flag-root");
    let env_root = home.path().join("env-root");
    // Enabled experts must sum to 100: this file breaks that, so a run that
    // reads it always fails.
    make_state_root(
        &flag_root,
        Some("[review_experts.lead]\nweight = 90\n\n[review_experts.security]\nweight = 15\n".to_string()),
    );
    make_state_root(&env_root, Some(config_toml(None)));

    let flag = flag_root.to_str().unwrap().to_string();
    let env = env_root.to_str().unwrap().to_string();

    for (name, variable) in [
        ("REVIEW_DATA_DIR", "REVIEW_DATA_DIR"),
        ("REVIEW_ENGINE_CONFIG_DIR", "REVIEW_ENGINE_CONFIG_DIR"),
        ("the home default", ""),
    ] {
        let args = vec!["--config-dir", flag.as_str(), "validate"];
        let output = run_isolated(&args, cwd.path(), home.path(), &[(variable, env.as_str())]);
        assert!(
            !output.status.success(),
            "--config-dir must win over {name}: {}",
            stdout_of(&output)
        );
        assert!(
            stderr_of(&output).contains("sum to"),
            "--config-dir's own file must be the one that failed, got: {}",
            stderr_of(&output)
        );
    }

    // …and the flag is not simply ignoring the environment: with no flag, the
    // variable decides.
    let output = run_isolated(
        &["validate"],
        cwd.path(),
        home.path(),
        &[("REVIEW_ENGINE_CONFIG_DIR", &env)],
    );
    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    let output = run_isolated(&["validate"], cwd.path(), home.path(), &[("REVIEW_DATA_DIR", &env)]);
    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
}

/// `serve --data-dir` still outranks the global flag: the serve-specific flag
/// names the instance's root, and the global one is only a fallback for it.
///
/// The roots are chosen so that reading the wrong one is fatal — the flag's
/// root holds an invalid config — which makes "serve is running and wrote its
/// database into the `--data-dir` root" a proof of the precedence.
#[test]
fn serve_data_dir_outranks_the_config_dir_flag() {
    let home = TempDir::new().unwrap();
    let data_dir = home.path().join("data-dir");
    let flag_root = home.path().join("flag-root");
    make_state_root(
        &flag_root,
        Some("[review_experts.lead]\nweight = 90\n\n[review_experts.security]\nweight = 15\n".to_string()),
    );

    let mut child = isolated_command(
        &[
            "serve",
            "--port",
            &free_port().to_string(),
            "--data-dir",
            data_dir.to_str().unwrap(),
            "--config-dir",
            flag_root.to_str().unwrap(),
        ],
        home.path(),
        home.path(),
        &[],
    )
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("failed to spawn review-engine serve");

    let expected = data_dir.join("review.db");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !expected.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "serve exited instead of starting on --data-dir: {:?}",
            child.wait_with_output().map(|out| stderr_of(&out))
        );
        assert!(
            std::time::Instant::now() < deadline,
            "serve never wrote {} within 30s",
            expected.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait_with_output();

    assert!(
        !flag_root.join("review.db").exists(),
        "the state root must be the --data-dir one, not the --config-dir one"
    );
}

// ─── database over file ────────────────────────────────────────────

/// The user's scenario end to end: the state root's database provides the
/// provider chain, its stored primary heads it, the key is decrypted with the
/// directory's `secrets.key`, and the CLI leaves the database exactly as it
/// found it.
#[tokio::test]
async fn the_database_provides_the_provider_the_review_runs_on() {
    let home = TempDir::new().unwrap();
    let repo = repo_with_a_change();
    let root = home.path().join("nas-config");
    make_state_root(&root, Some(config_toml(None)));

    let primary = wiremock::MockServer::start().await;
    let other = wiremock::MockServer::start().await;
    mount_mock_llm(&primary, findings_body("DB provider reached the review", "sk-from-db")).await;
    mount_mock_llm(&other, findings_body("the non-primary provider was used", "sk-from-db")).await;

    seed_state_root_database(
        &root,
        &[
            db_card("mock-primary", &primary.uri(), "sk-from-db"),
            db_card("mock-other", &other.uri(), "sk-from-db"),
        ],
        Some("mock-primary"),
    )
    .await;
    let before = db_fingerprint(&root);

    let output = run_isolated(
        &[
            "--config-dir",
            root.to_str().unwrap(),
            "review",
            "--local-path",
            ".",
            "--base",
            "main",
            "--format",
            "json",
        ],
        repo.path(),
        home.path(),
        &[],
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        stdout_of(&output),
        stderr_of(&output)
    );
    assert!(
        stdout_of(&output).contains("DB provider reached the review"),
        "the database's provider must have been the one that answered: {}",
        stdout_of(&output)
    );

    // The persisted `ui.llm.primaryProvider` heads the chain (RENG-55/75): the
    // second provider must never have been called.
    assert!(
        !authorization_headers(&primary).await.is_empty(),
        "the primary provider must have been called"
    );
    assert_eq!(
        other.received_requests().await.unwrap().len(),
        0,
        "the persisted primary is the head of the chain; nothing else may be tried first"
    );
    assert!(
        authorization_headers(&primary)
            .await
            .iter()
            .all(|header| header == "Bearer sk-from-db"),
        "the API key must be the database's, decrypted with the config dir's secrets.key"
    );

    // And the CLI did not write the configuration database: same bytes, same
    // mtime, and no `-wal`/`-shm` minted next to it.
    assert_eq!(
        db_fingerprint(&root),
        before,
        "the CLI must not touch review.db (or create its sidecars)"
    );
}

/// No database → the config files are the whole configuration, and the review
/// runs on the file's provider. This is the behaviour every existing
/// deployment keeps.
#[tokio::test]
async fn without_a_database_the_review_runs_on_the_config_file() {
    let home = TempDir::new().unwrap();
    let repo = repo_with_a_change();
    let root = home.path().join("nas-config");

    let mock = wiremock::MockServer::start().await;
    mount_mock_llm(
        &mock,
        findings_body("config-file provider reached the review", "sk-from-toml"),
    )
    .await;
    make_state_root(
        &root,
        Some(config_toml(Some((
            "mock-toml",
            &format!("{}/v1", mock.uri()),
            "sk-from-toml",
        )))),
    );
    assert!(!root.join(review_engine::paths::DB_FILE_NAME).exists());

    let output = run_isolated(
        &[
            "--config-dir",
            root.to_str().unwrap(),
            "review",
            "--local-path",
            ".",
            "--base",
            "main",
            "--format",
            "json",
        ],
        repo.path(),
        home.path(),
        &[],
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        stdout_of(&output),
        stderr_of(&output)
    );
    assert!(stdout_of(&output).contains("config-file provider reached the review"));
    assert_eq!(
        authorization_headers(&mock).await.first().map(String::as_str),
        Some("Bearer sk-from-toml"),
        "the run must have used the config file's provider"
    );
    assert!(
        !root.join(review_engine::paths::DB_FILE_NAME).exists(),
        "a read must never create a database"
    );
}

/// A `review.db` that cannot be read (here: not a database at all) degrades to
/// the config files with exactly ONE warning — never a failed command, and
/// never a silently different configuration.
#[tokio::test]
async fn an_unreadable_database_warns_once_and_falls_back_to_the_config_files() {
    let home = TempDir::new().unwrap();
    let repo = repo_with_a_change();
    let root = home.path().join("nas-config");

    let mock = wiremock::MockServer::start().await;
    mount_mock_llm(
        &mock,
        findings_body("config-file provider reached the review", "sk-from-toml"),
    )
    .await;
    make_state_root(
        &root,
        Some(config_toml(Some((
            "mock-toml",
            &format!("{}/v1", mock.uri()),
            "sk-from-toml",
        )))),
    );
    // The key exists (the open gets past the key check) but the database is
    // garbage: a truncated copy, or an unrelated file in the database's place.
    std::fs::write(
        root.join(review_engine::config::secrets::SECRETS_KEY_FILE_NAME),
        [3u8; 32],
    )
    .unwrap();
    std::fs::write(
        root.join(review_engine::paths::DB_FILE_NAME),
        b"this is not a sqlite database",
    )
    .unwrap();
    let before = db_fingerprint(&root);

    let output = run_isolated(
        &[
            "--config-dir",
            root.to_str().unwrap(),
            "review",
            "--local-path",
            ".",
            "--base",
            "main",
            "--format",
            "json",
        ],
        repo.path(),
        home.path(),
        &[],
    );
    assert!(
        output.status.success(),
        "an unreadable database must not block the command: stdout: {}\nstderr: {}",
        stdout_of(&output),
        stderr_of(&output)
    );
    assert!(
        stdout_of(&output).contains("config-file provider reached the review"),
        "the config files must be what the run used: {}",
        stdout_of(&output)
    );
    let stderr = stderr_of(&output);
    let warnings = stderr
        .lines()
        .filter(|line| line.contains("ignoring the configuration database"))
        .count();
    assert_eq!(
        warnings, 1,
        "exactly one warning, not a line per surface — stderr: {stderr}"
    );
    assert!(
        stderr.contains("review.db"),
        "the warning must name the database it ignored: {stderr}"
    );
    assert_eq!(
        db_fingerprint(&root),
        before,
        "the degraded path must not touch the database either"
    );
}

/// RENG-107 r2 — the reviewer's P2-1 repro: `review --config <file>` with a
/// concurrency cap of 0.
///
/// `--config` is read as a WHOLE `AppConfig` (`ConfigSource::Path` →
/// `load_config_without_llm` → `load_and_apply`), unlike the auto-detected
/// `.code-audit-config.toml`, whose top-level scalars the resolver drops. So the
/// 0 reached `Semaphore::new(0)` and the command hung: rc=124 under `timeout`,
/// one LLM request (the pre-semaphore global-context call), no output at all —
/// the silent-hang class, reachable through a documented flag.
///
/// The CLI now clears a 0 cap and warns once, so the review runs on the file's
/// provider and the pipeline default.
#[tokio::test]
async fn a_zero_concurrency_cap_in_an_explicit_config_file_does_not_hang() {
    let home = TempDir::new().unwrap();
    let repo = repo_with_a_change();
    let root = home.path().join("nas-config");
    // No config file and no database in the state root: the only provider is
    // the one in the `--config` file under test.
    make_state_root(&root, None);

    let mock = wiremock::MockServer::start().await;
    mount_mock_llm(&mock, findings_body("the review still ran", "sk-from-cfg")).await;

    let cfg = home.path().join("cfg.toml");
    std::fs::write(
        &cfg,
        format!(
            "max_concurrent_llm_calls = 0\nmax_team_size = 0\n{}",
            config_toml(Some(("mock-cfg", &format!("{}/v1", mock.uri()), "sk-from-cfg")))
        ),
    )
    .unwrap();

    let run = run_with_deadline(
        &[
            "--config-dir",
            root.to_str().unwrap(),
            "review",
            "--local-path",
            ".",
            "--base",
            "main",
            "--format",
            "json",
            "--config",
            cfg.to_str().unwrap(),
        ],
        repo.path(),
        home.path(),
        60,
    );

    assert!(
        run.success,
        "a 0 cap must not hang the command — stdout: {}\nstderr: {}",
        run.stdout, run.stderr
    );
    assert!(
        run.stdout.contains("the review still ran"),
        "the review must have run to completion: {}",
        run.stdout
    );
    assert_eq!(
        authorization_headers(&mock).await.first().map(String::as_str),
        Some("Bearer sk-from-cfg"),
        "and it must have used the --config file's provider"
    );

    assert_eq!(
        run.stderr.lines().filter(|line| line.contains("limit of 0")).count(),
        1,
        "exactly one warning about the ignored cap — stderr: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("max_concurrent_llm_calls") && run.stderr.contains("max_team_size"),
        "the warning must name both keys it ignored: {}",
        run.stderr
    );
}
