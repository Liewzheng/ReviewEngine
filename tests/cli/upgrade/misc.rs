use super::*;

// ─────────────────────────────────────────────────────────────────────
// `ask --stdin` — AK-05 regression + M34 dual-write pin.
//
// `run_ask_stdin` used to call the file-backed handler with the arguments
// swapped: the stdin diff was passed as `question` and the question string as
// `diff_path`, so any non-empty stdin + `--question` treated the question as
// a file path and failed with `No such file or directory (os error 2)`.
// The stdin path now runs against the in-memory diff; this test drives the
// real binary with a mock LLM and asserts the happy path succeeds.
//
// M34 (W13): the `ask` / `describe` / `improve` / `update_changelog`
// handlers now also write a timestamped copy of the report under
// `config.output_dir` (default `<state dir>/reports/`) — the same dual-write
// the `review` and `repo-review` paths already do. The pre-M34 handlers
// passed `None` for the output dir, so ask/describe/improve silently
// dropped the copy and the docs in `docs/getting-started.md` only applied
// to the review commands. This test pins the new behaviour: an isolated
// `--config-dir` state root guarantees the timestamped copy lands in a
// fresh temp directory and never touches the user's real config.
//
// Two operational notes worth keeping here:
//   (a) every ask/describe/improve/update_changelog run now writes into
//       `<state dir>/reports/`, which on a default install is
//       `~/.config/review-engine/reports/` — CI runs and shell wrappers
//       inherit `HOME`, so they write there too;
//   (b) an unwritable or unresolvable `output_dir` now makes these
//       commands exit non-zero (the dual write fails after stdout already
//       received the report). The `review` commands have always behaved
//       this way; the three new ones inherit it. The current behaviour
//       is consistent with `review` and is the least surprising choice
//       (a silent drop would be a regression on the docs promise), so
//       this test does not paper over it.
// ─────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn ask_stdin_with_question_succeeds_with_mock_llm() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": "stdin diff answer"}}],
            "model": "mock-model",
            "usage": {"total_tokens": 7}
        })))
        .mount(&server)
        .await;

    let llm_config = serde_json::json!({
        "provider": "mock",
        "model": "mock-model",
        "api_key": "k",
        "api_base": format!("{}/v1", server.uri()),
    })
    .to_string();

    let diff = "diff --git a/src/main.rs b/src/main.rs\n\
                index 0000000..1111111 100644\n\
                --- a/src/main.rs\n\
                +++ b/src/main.rs\n\
                @@ -1 +1 @@\n\
                -fn old() {}\n\
                +fn new() {}\n";

    // Isolated state root: a fresh temp HOME plus --config-dir pointing at a
    // sibling temp, with every steering env var cleared. Without isolation
    // the timestamped-copy assertion below would either write into the
    // user's real `~/.config/review-engine/reports/` (RENG-113: 0600 file
    // ownership stays with the user, so the test would leak there) or pick
    // up a REVIEW_ENGINE_CONFIG_DIR from the developer's shell.
    let home = TempDir::new().unwrap();
    let state_root = TempDir::new().unwrap();
    let state_root_str = state_root.path().to_str().unwrap().to_string();

    let mut cmd = Command::new(bin_path());
    cmd.args([
        "--config-dir",
        &state_root_str,
        "ask",
        "--stdin",
        "--question",
        "What does this diff change?",
        "--format",
        "markdown",
        "--llm-config",
        llm_config.as_str(),
    ])
    .env("HOME", home.path())
    .current_dir(home.path());
    for key in [
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
    ] {
        cmd.env_remove(key);
    }
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn review-engine ask --stdin");

    use std::io::Write;
    child
        .stdin
        .take()
        .expect("stdin pipe missing")
        .write_all(diff.as_bytes())
        .expect("failed to write diff to stdin");
    // Dropping the writer closes stdin, which the child reads to EOF.

    let output = child.wait_with_output().expect("failed to collect output");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "ask --stdin must succeed, status={:?}\nstdout: {stdout}\nstderr: {stderr}",
        output.status.code()
    );
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        !combined.contains("os error 2"),
        "ask --stdin must not hit os error 2, got: {combined}"
    );
    assert!(
        stdout.contains("stdin diff answer"),
        "expected the mock LLM answer on stdout, got: {stdout}"
    );

    // M34 dual-write pin: a timestamped copy of the same report must land
    // under `<state dir>/reports/`. Pre-M34, ask passed `None` for the
    // output dir, so this directory stayed empty and the docs in
    // `docs/getting-started.md` only applied to the review paths.
    let reports_dir = state_root.path().join("reports");
    let saved: Vec<_> = std::fs::read_dir(&reports_dir)
        .unwrap_or_else(|e| {
            panic!(
                "ask --stdin must create the reports dir under --config-dir ({reports_dir:?}): {e}\nstdout: {stdout}\nstderr: {stderr}"
            )
        })
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert_eq!(
        saved.len(),
        1,
        "exactly one timestamped copy under {reports_dir:?}: {saved:?}"
    );
    let name = saved[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("review_") && name.ends_with(".md"),
        "timestamped copy name must match the review_<ts>.<ext> pattern, got: {name}"
    );
    let file_content = std::fs::read_to_string(&saved[0]).unwrap();
    // The sink prints to stdout via `println!` (one trailing newline) and
    // writes the raw `text` to the file (which itself ends with one
    // newline from the ask markdown format), so the two are equal up to
    // one trailing newline. Compare after trimming all trailing
    // whitespace from both sides.
    assert_eq!(
        file_content.trim_end(),
        stdout.trim_end(),
        "timestamped copy must mirror the stdout report (modulo trailing whitespace)"
    );
}

// ─────────────────────────────────────────────────────────────────────
// `audit --local-path <missing>` — RR-03 regression.
//
// `RepoScanner::scan_dir` used to fail open on a non-existent root (empty
// entries → a fabricated 53/100 report). It must now fail closed with a clear
// error and a non-zero exit.
// ─────────────────────────────────────────────────────────────────────
#[test]
fn audit_nonexistent_local_path_errors() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("does-not-exist");

    let output = run(
        &["audit", "--local-path", missing.to_str().unwrap(), "--format", "json"],
        Some(dir.path()),
    );
    assert!(
        !output.status.success(),
        "audit on a missing --local-path must exit non-zero: {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Repository path does not exist"),
        "stderr must name the missing path, got: {stderr}"
    );
}
