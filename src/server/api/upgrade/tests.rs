use super::start::validate_origin;
use super::task::{
    current_exe_name, exit_after_upgrade_enabled, find_dist_root, install_method_str, replace_frontend_dist,
    resolve_frontend_dir, resolve_install_dir,
};
use crate::server::AppState;
use crate::upgrade::{InstallMethod, Release};
use axum::http::HeaderMap;
use std::path::PathBuf;

#[test]
fn install_method_mapping_matches_contract() {
    assert_eq!(install_method_str(InstallMethod::Plain), "binary");
    assert_eq!(install_method_str(InstallMethod::Brew), "brew");
    assert_eq!(install_method_str(InstallMethod::Docker), "docker");
    assert_eq!(install_method_str(InstallMethod::Cargo), "cargo");
    assert_eq!(install_method_str(InstallMethod::Unknown), "unknown");
}

#[test]
fn install_dir_resolution_override_then_canonical() {
    let saved = std::env::var("REVIEW_UPGRADE_INSTALL_DIR").ok();

    std::env::set_var("REVIEW_UPGRADE_INSTALL_DIR", "/tmp/reng-upgrade-test");
    assert_eq!(resolve_install_dir(), PathBuf::from("/tmp/reng-upgrade-test"));

    std::env::remove_var("REVIEW_UPGRADE_INSTALL_DIR");
    let dir = resolve_install_dir();
    assert!(dir.is_absolute(), "install dir must be absolute, got {dir:?}");
    assert!(!current_exe_name().is_empty());

    match saved {
        Some(v) => std::env::set_var("REVIEW_UPGRADE_INSTALL_DIR", v),
        None => {}
    }
}

// ─── Origin validation (B2) ────────────────────────────────

fn headers_with(origin: Option<&str>, host: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(o) = origin {
        headers.insert("origin", o.parse().expect("valid origin header value"));
    }
    if let Some(h) = host {
        headers.insert("host", h.parse().expect("valid host header value"));
    }
    headers
}

#[test]
fn origin_none_passes() {
    assert!(validate_origin(&headers_with(None, Some("127.0.0.1:8080"))).is_ok());
    assert!(validate_origin(&HeaderMap::new()).is_ok());
}

#[test]
fn origin_same_authority_passes() {
    assert!(validate_origin(&headers_with(Some("http://127.0.0.1:8080"), Some("127.0.0.1:8080"))).is_ok());
    assert!(validate_origin(&headers_with(Some("https://localhost:5173"), Some("localhost:5173"))).is_ok());
    assert!(validate_origin(&headers_with(Some("http://example.com"), Some("example.com"))).is_ok());
}

#[test]
fn origin_cross_site_rejected() {
    for (origin, host) in [
        ("http://evil.example", "127.0.0.1:8080"),
        ("http://127.0.0.1:9999", "127.0.0.1:8080"),
        ("https://evil.example", "localhost:5173"),
    ] {
        let err = validate_origin(&headers_with(Some(origin), Some(host))).expect_err("must reject");
        let status = err.status();
        assert_eq!(
            status,
            axum::http::StatusCode::FORBIDDEN,
            "origin {origin} vs host {host}"
        );
    }
    assert!(validate_origin(&headers_with(Some("http://evil.example"), None)).is_err());
}

// ─── frontend dist (container upgrade) ─────────────────────

#[test]
fn resolve_frontend_dir_override_then_default() {
    let saved = std::env::var("REVIEW_UPGRADE_FRONTEND_DIR").ok();

    std::env::set_var("REVIEW_UPGRADE_FRONTEND_DIR", "/tmp/reng-frontend-test");
    assert_eq!(resolve_frontend_dir(), PathBuf::from("/tmp/reng-frontend-test"));

    std::env::remove_var("REVIEW_UPGRADE_FRONTEND_DIR");
    assert_eq!(resolve_frontend_dir(), PathBuf::from("/app/frontend/dist"));

    match saved {
        Some(v) => std::env::set_var("REVIEW_UPGRADE_FRONTEND_DIR", v),
        None => {}
    }
}

#[test]
fn find_dist_root_flat_nested_and_missing() {
    let dir = tempfile::tempdir().expect("temp dir");

    let flat = dir.path().join("flat");
    std::fs::create_dir_all(&flat).expect("create flat");
    std::fs::write(flat.join("index.html"), "<html></html>").expect("write index.html");
    std::fs::write(flat.join("app.js"), "console.log(1)").expect("write app.js");
    assert_eq!(find_dist_root(&flat), Some(flat.clone()));

    let nested = dir.path().join("nested").join("frontend").join("dist");
    std::fs::create_dir_all(&nested).expect("create nested");
    std::fs::write(nested.join("index.html"), "<html></html>").expect("write nested index.html");
    let nested_root = dir.path().join("nested");
    assert_eq!(find_dist_root(&nested_root), Some(nested));

    let empty = dir.path().join("empty");
    std::fs::create_dir_all(empty.join("assets")).expect("create empty");
    assert!(find_dist_root(&empty).is_none());
}

#[test]
fn replace_frontend_dist_happy_path_and_rollback() {
    let dir = tempfile::tempdir().expect("temp dir");
    let live = dir.path().join("frontend").join("dist");
    std::fs::create_dir_all(&live).expect("create live");
    std::fs::write(live.join("old.txt"), "old").expect("write old");
    let staged = dir.path().join("staged-dist");
    std::fs::create_dir_all(&staged).expect("create staged");
    std::fs::write(staged.join("index.html"), "<html>new</html>").expect("write new index.html");
    std::fs::create_dir_all(staged.join("assets")).expect("create staged assets");
    std::fs::write(staged.join("assets/app.js"), "console.log(1)").expect("write staged asset");

    replace_frontend_dist(&staged, &live).expect("replace must succeed");
    assert!(live.join("index.html").exists(), "new dist must be live");
    assert!(live.join("assets/app.js").exists(), "nested asset must be live");
    assert!(!live.join("old.txt").exists(), "old dist must be gone");
    let leftovers: Vec<_> = std::fs::read_dir(&live)
        .expect("read live dir")
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(".new-") || n.starts_with(".old-")
        })
        .collect();
    assert!(leftovers.is_empty(), "temp dirs must be cleaned up, got {leftovers:?}");

    let live2 = dir.path().join("frontend2").join("dist");
    std::fs::create_dir_all(&live2).expect("create live2");
    std::fs::write(live2.join("old.txt"), "old").expect("write old2");
    let missing_staged = dir.path().join("does-not-exist-dist");
    let err = replace_frontend_dist(&missing_staged, &live2).expect_err("missing staged must fail");
    assert!(matches!(err, crate::upgrade::UpgradeError::Io(_)), "got {err:?}");
    assert!(live2.join("old.txt").exists(), "old dist must be restored on failure");
}

#[cfg(unix)]
#[test]
fn replace_frontend_dist_keeps_live_dir_on_mount_point_semantics() {
    use std::os::unix::fs::MetadataExt;

    let dir = tempfile::tempdir().expect("temp dir");
    let live = dir.path().join("dist");
    std::fs::create_dir_all(&live).expect("create live");
    std::fs::write(live.join("old.txt"), "old").expect("write old");
    let staged = dir.path().join("staged");
    std::fs::create_dir_all(&staged).expect("create staged");
    std::fs::write(staged.join("index.html"), "<html>new</html>").expect("write index.html");

    let before = std::fs::metadata(&live).expect("metadata before");
    replace_frontend_dist(&staged, &live).expect("replace must succeed");
    let after = std::fs::metadata(&live).expect("metadata after");

    assert_eq!(before.dev(), after.dev(), "live dir device must not change");
    assert_eq!(
        before.ino(),
        after.ino(),
        "live dir inode must not change — the directory itself must never be renamed/replaced (EBUSY on a mount point)"
    );
    assert!(live.join("index.html").exists());
    assert!(!live.join("old.txt").exists());
}

#[test]
fn replace_frontend_dist_accepts_staged_in_independent_location() {
    let staged_root = tempfile::tempdir().expect("staged root");
    let staged = staged_root.path().join("dist");
    std::fs::create_dir_all(staged.join("assets")).expect("create staged");
    std::fs::write(staged.join("index.html"), "<html>new</html>").expect("write index.html");
    std::fs::write(staged.join("assets/app.js"), "console.log(1)").expect("write asset");

    let live_root = tempfile::tempdir().expect("live root");
    let live = live_root.path().join("dist");
    std::fs::create_dir_all(&live).expect("create live");
    std::fs::write(live.join("old.txt"), "old").expect("write old");

    replace_frontend_dist(&staged, &live).expect("replace across independent locations must succeed");
    assert!(live.join("index.html").exists());
    assert!(live.join("assets/app.js").exists());
    assert!(!live.join("old.txt").exists());
    assert!(
        staged.join("index.html").exists(),
        "staged source must not be consumed by the replace (copy, not move)"
    );
}

#[test]
fn stage_frontend_dist_returns_none_when_asset_missing() {
    let release = Release {
        tag_name: "v9.9.9".to_string(),
        html_url: "https://example.com".to_string(),
        published_at: "2026-01-01T00:00:00Z".to_string(),
        assets: vec![crate::upgrade::ReleaseAsset {
            name: "review-engine-x86_64-unknown-linux-gnu.tar.gz".to_string(),
            download_url: "https://example.com/binary".to_string(),
            size: 1,
        }],
    };
    let state = AppState::new(vec![]);
    let staging = tempfile::tempdir().expect("temp dir");
    let staged = tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(super::task::stage_frontend_dist(&state, &release, staging.path()));
    assert!(
        matches!(staged, Ok(None)),
        "no dist asset must degrade to Ok(None), got {staged:?}"
    );
    assert_eq!(
        std::fs::read_dir(staging.path()).expect("read staging").count(),
        0,
        "nothing may be downloaded when the dist asset is absent"
    );
}

#[test]
fn exit_after_upgrade_gate_honors_env() {
    let saved = std::env::var("REVIEW_UPGRADE_EXIT_AFTER").ok();

    std::env::set_var("REVIEW_UPGRADE_EXIT_AFTER", "0");
    assert!(!exit_after_upgrade_enabled(), "0 must disable the exit");

    std::env::set_var("REVIEW_UPGRADE_EXIT_AFTER", "1");
    assert!(exit_after_upgrade_enabled(), "1 must keep the exit");

    std::env::remove_var("REVIEW_UPGRADE_EXIT_AFTER");
    assert!(
        exit_after_upgrade_enabled(),
        "unset must default to exiting (production)"
    );

    match saved {
        Some(v) => std::env::set_var("REVIEW_UPGRADE_EXIT_AFTER", v),
        None => {}
    }
}
