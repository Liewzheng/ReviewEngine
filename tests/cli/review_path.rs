use super::*;

// ─────────────────────────────────────────────────────────────────────
// `review --path <dir>` — P0 full-content directory review.
//
// The subdirectory's controlled files are reviewed in full through a
// synthetic "empty tree → current" diff. The mock LLM reports one finding
// per file at line 1; `validate_findings` drops any finding whose file is
// NOT in the diff, so surviving findings prove every file was covered.
// ─────────────────────────────────────────────────────────────────────

/// YAML findings body: one finding per file at line 1 (any line is inside
/// the whole-file hunk of a synthetic new-file diff).
fn coverage_findings_yaml(files: &[&str]) -> String {
    let mut out = String::from("review:\n  findings:\n");
    for f in files {
        out.push_str(&format!(
            "    - file: \"{f}\"\n      line: 1\n      severity: \"high\"\n      title: \"Coverage: {f}\"\n      detail: \"Ensures {f} is in the reviewed diff\"\n"
        ));
    }
    out
}

/// Mount a mock OpenAI-compatible `/v1/chat/completions` returning `body`.
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

fn mock_llm_config(server_uri: &str) -> String {
    serde_json::json!({
        "provider": "mock",
        "model": "mock-model",
        "api_key": "k",
        "api_base": format!("{}/v1", server_uri),
    })
    .to_string()
}

#[tokio::test]
async fn review_path_covers_all_files_with_mock_llm() {
    let dir = TempDir::new().unwrap();
    let repo_path = dir.path();
    git_init(repo_path);
    git_config_user(repo_path);

    let files = ["lib/camera.c", "lib/camera.h", "lib/sub/driver.c"];
    for f in files {
        let p = repo_path.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, format!("// {f}\nint fn_1(void) {{ return 0; }}\n")).unwrap();
    }
    git_add_and_commit(repo_path, "initial commit");

    let server = wiremock::MockServer::start().await;
    mount_mock_llm(&server, coverage_findings_yaml(&files)).await;

    let output = run(
        &[
            "review",
            "--path",
            "lib",
            "--local-path",
            ".",
            "--format",
            "json",
            "--llm-config",
            &mock_llm_config(&server.uri()),
        ],
        Some(repo_path),
    );

    assert!(
        output.status.success(),
        "review --path failed: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("review --path stdout is not valid JSON");
    let empty = vec![];
    let mut reviewed: Vec<String> = value["reports"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .flat_map(|r| r["findings"].as_array().unwrap_or(&empty).iter())
        .filter_map(|f| f["file"].as_str().map(String::from))
        .collect();
    reviewed.sort();
    reviewed.dedup();

    for f in files {
        assert!(
            reviewed.iter().any(|r| r == f),
            "file {f} was not covered by the full-content review; covered={reviewed:?}"
        );
    }
}

#[tokio::test]
async fn review_path_zero_findings_appends_credibility_note() {
    let dir = TempDir::new().unwrap();
    let repo_path = dir.path();
    git_init(repo_path);
    git_config_user(repo_path);

    let lib = repo_path.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(lib.join("only.c"), "int fn_1(void) { return 0; }\n").unwrap();
    git_add_and_commit(repo_path, "initial commit");

    let server = wiremock::MockServer::start().await;
    mount_mock_llm(&server, "review:\n  findings: []\n".to_string()).await;

    let output = run(
        &[
            "review",
            "--path",
            "lib",
            "--local-path",
            ".",
            "--format",
            "markdown",
            "--llm-config",
            &mock_llm_config(&server.uri()),
        ],
        Some(repo_path),
    );

    assert!(
        output.status.success(),
        "review --path failed: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("零发现不代表代码无问题"),
        "zero-findings full review must append the credibility note, got: {stdout}"
    );
    assert!(
        stdout.contains("1 file(s)") && stdout.contains("`lib`"),
        "note must state the reviewed file count and path, got: {stdout}"
    );
}

#[test]
fn review_path_missing_dir_errors() {
    let dir = TempDir::new().unwrap();
    let output = run(
        &["review", "--path", "nope", "--local-path", ".", "--format", "json"],
        Some(dir.path()),
    );
    assert!(!output.status.success(), "missing dir must fail: {:?}", output.status);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Directory to review does not exist"),
        "stderr must name the missing directory, got: {stderr}"
    );
}

#[test]
fn review_path_empty_dir_errors() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("empty")).unwrap();
    let output = run(
        &["review", "--path", "empty", "--local-path", ".", "--format", "json"],
        Some(dir.path()),
    );
    assert!(!output.status.success(), "empty dir must fail: {:?}", output.status);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no reviewable files"),
        "stderr must report no reviewable files, got: {stderr}"
    );
}

#[test]
fn review_path_rejects_traversal() {
    let dir = TempDir::new().unwrap();
    let output = run(
        &["review", "--path", "../escape", "--local-path", ".", "--format", "json"],
        Some(dir.path()),
    );
    assert!(!output.status.success(), "traversal must fail: {:?}", output.status);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--path"),
        "stderr must reject the traversal path, got: {stderr}"
    );
}

#[test]
fn review_path_conflicts_with_other_input_sources() {
    let dir = TempDir::new().unwrap();
    let output = run(
        &[
            "review",
            "--path",
            "lib",
            "--local-path",
            ".",
            "--base",
            "main",
            "--format",
            "json",
        ],
        Some(dir.path()),
    );
    assert!(!output.status.success(), "--path + --base must be rejected");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("cannot be used with") || stderr.contains("conflict"),
        "stderr must explain the conflict, got: {stderr}"
    );
}

#[test]
fn review_path_requires_local_path() {
    let dir = TempDir::new().unwrap();
    let output = run(&["review", "--path", "lib"], Some(dir.path()));
    assert!(!output.status.success(), "--path without --local-path must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("--local-path"),
        "stderr must require --local-path, got: {stderr}"
    );
}
