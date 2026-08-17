use super::*;

// ─────────────────────────────────────────────────────────────────────
// `ask --stdin` — AK-05 regression.
//
// `run_ask_stdin` used to call the file-backed handler with the arguments
// swapped: the stdin diff was passed as `question` and the question string as
// `diff_path`, so any non-empty stdin + `--question` treated the question as
// a file path and failed with `No such file or directory (os error 2)`.
// The stdin path now runs against the in-memory diff; this test drives the
// real binary with a mock LLM and asserts the happy path succeeds.
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

    let mut child = Command::new(bin_path())
        .args([
            "ask",
            "--stdin",
            "--question",
            "What does this diff change?",
            "--format",
            "markdown",
            "--llm-config",
            llm_config.as_str(),
        ])
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
