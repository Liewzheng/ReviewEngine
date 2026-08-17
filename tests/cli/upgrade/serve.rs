use super::*;

// ─────────────────────────────────────────────────────────────────────
// `serve` subcommand — startup contract (v0.9.2 silent-hang defect fix):
//   * occupied port → fail FAST: non-zero exit + stderr naming the
//     conflict ("Address already in use (port N)") — never a silent hang.
//     Regression root cause: the config watcher's `spawn_blocking` task
//     parks on a never-ready `mpsc::recv`, which blocks tokio runtime
//     teardown forever, so a bind error never reached the terminal.
//   * free port → one-line stdout startup banner (listen address + health
//     URL + log file path) and `/health` answers 200.
// Both tests run with HOME pointed at a TempDir so user-level config and
// logs.ndjson never leak into the assertion surface.
// ─────────────────────────────────────────────────────────────────────

/// Spawn the binary and wait for it to exit, killing it after `timeout`.
/// Panics (after killing the child) when the process hangs — a regression
/// must fail the test fast instead of blocking the whole suite forever.
fn run_with_timeout(args: &[&str], home: &Path, timeout: std::time::Duration) -> std::process::Output {
    let mut child = Command::new(bin_path())
        .args(args)
        .env("HOME", home)
        .current_dir(home)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn review-engine");
    let start = std::time::Instant::now();
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            return child.wait_with_output().expect("failed to collect output");
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait_with_output();
            panic!("review-engine {args:?} did not exit within {timeout:?} — silent-hang regression");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Find an ephemeral free port by binding and immediately releasing :0.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("failed to bind ephemeral port");
    listener.local_addr().expect("no local addr").port()
}

#[test]
fn serve_fails_fast_when_port_in_use() {
    let home = TempDir::new().unwrap();
    // Occupy the target port for the whole lifetime of the child run.
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").expect("failed to bind blocker");
    let port = blocker.local_addr().unwrap().port();

    let output = run_with_timeout(
        &["serve", "--port", &port.to_string()],
        home.path(),
        std::time::Duration::from_secs(30),
    );

    assert!(
        !output.status.success(),
        "serve on an occupied port must exit non-zero: {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Address already in use"),
        "stderr must name the conflict, got: {stderr}"
    );
    assert!(
        stderr.contains(&port.to_string()),
        "stderr must name the occupied port {port}, got: {stderr}"
    );
    drop(blocker);
}

#[test]
fn serve_prints_startup_banner_and_serves_health() {
    let home = TempDir::new().unwrap();
    let port = free_port();

    let mut child = Command::new(bin_path())
        .args(["serve", "--port", &port.to_string()])
        .env("HOME", home.path())
        .current_dir(home.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn review-engine serve");

    // Poll /health until the server answers (or fail within 30s).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let health_response = loop {
        match std::net::TcpStream::connect(("127.0.0.1", port)) {
            Ok(mut stream) => {
                use std::io::{Read, Write};
                stream
                    .write_all(b"GET /health HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                    .expect("failed to write health request");
                let mut buf = String::new();
                stream.read_to_string(&mut buf).expect("failed to read health response");
                break buf;
            }
            Err(e) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let _ = e;
            }
            Err(e) => {
                let _ = child.kill();
                panic!("serve did not open port {port} within 30s: {e}");
            }
        }
    };
    assert!(
        health_response.contains("200") && health_response.contains("\"status\":\"ok\""),
        "unexpected /health response: {health_response}"
    );

    // Give the line-buffered banner a moment to flush, then stop the server.
    std::thread::sleep(std::time::Duration::from_millis(500));
    let _ = child.kill();
    let output = child.wait_with_output().expect("failed to collect output");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("listening on"),
        "stdout must carry the startup banner, got: {stdout}"
    );
    assert!(
        stdout.contains(&format!("127.0.0.1:{port}")),
        "banner must name the listen address, got: {stdout}"
    );
    assert!(
        stdout.contains("/health"),
        "banner must name the health-check URL, got: {stdout}"
    );
    assert!(
        stdout.contains("logs.ndjson"),
        "banner must name the log file path, got: {stdout}"
    );
}
