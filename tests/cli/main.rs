#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn bin_path() -> String {
    std::env::var("CARGO_BIN_EXE_review-engine").unwrap_or_else(|_| "target/debug/review-engine".to_string())
}

fn run(args: &[&str], current_dir: Option<&Path>) -> std::process::Output {
    let mut cmd = Command::new(bin_path());
    cmd.args(args);
    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }
    cmd.output().expect("failed to execute review-engine")
}

fn git_init(path: &Path) {
    run_git(path, &["init"])
}

fn git_config_user(path: &Path) {
    run_git(path, &["config", "user.email", "test@example.com"]);
    run_git(path, &["config", "user.name", "Test User"]);
}

fn git_add_and_commit(path: &Path, message: &str) {
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", message]);
}

fn run_git(path: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(path)
        .status()
        .expect("git is not available");
    assert!(status.success(), "git command failed: git {:?}", args);
}

mod basic;
mod review_path;
mod upgrade;
