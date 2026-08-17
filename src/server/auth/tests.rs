use super::*;
use axum::http::Request;

#[test]
fn test_generate_token_format() {
    let token = generate_token();
    assert!(token.starts_with("review_"));
    assert_eq!(token.len(), 32 + 7); // "review_" + 32 hex chars (16 bytes * 2)
}

#[test]
fn test_auth_config_local_addr_no_token_ok() {
    let config = AuthConfig::new(None, "127.0.0.1");
    assert!(config.is_ok());
}

#[test]
fn test_auth_config_non_local_addr_requires_token() {
    let config = AuthConfig::new(None, "0.0.0.0");
    assert!(config.is_err());
    let err = config.unwrap_err().to_string();
    assert!(err.contains("requires an API token"));
}

#[test]
fn test_auth_config_non_local_addr_with_token_ok() {
    let config = AuthConfig::new(Some("my-secret-token".to_string()), "0.0.0.0");
    assert!(config.is_ok());
}

#[test]
fn test_auth_check_disabled_always_true() {
    let config = AuthConfig::new(None, "127.0.0.1").unwrap();
    assert!(!config.is_enabled());
    let req = Request::builder().uri("/").body(axum::body::Body::empty()).unwrap();
    assert!(config.check(&req));
}

#[test]
fn test_auth_check_valid_bearer_token() {
    let config = AuthConfig::new(Some("secret123".to_string()), "0.0.0.0").unwrap();
    assert!(config.is_enabled());
    let req = Request::builder()
        .uri("/")
        .header("Authorization", "Bearer secret123")
        .body(axum::body::Body::empty())
        .unwrap();
    assert!(config.check(&req));
}

#[test]
fn test_auth_check_invalid_bearer_token() {
    let config = AuthConfig::new(Some("secret123".to_string()), "0.0.0.0").unwrap();
    let req = Request::builder()
        .uri("/")
        .header("Authorization", "Bearer wrong-token")
        .body(axum::body::Body::empty())
        .unwrap();
    assert!(!config.check(&req));
}

#[test]
fn test_auth_check_valid_x_api_key() {
    let config = AuthConfig::new(Some("secret123".to_string()), "0.0.0.0").unwrap();
    let req = Request::builder()
        .uri("/")
        .header("X-API-Key", "secret123")
        .body(axum::body::Body::empty())
        .unwrap();
    assert!(config.check(&req));
}

#[test]
fn test_auth_check_no_auth_header() {
    let config = AuthConfig::new(Some("secret123".to_string()), "0.0.0.0").unwrap();
    let req = Request::builder().uri("/").body(axum::body::Body::empty()).unwrap();
    assert!(!config.check(&req));
}

#[test]
fn test_auth_check_wrong_length_token() {
    let config = AuthConfig::new(Some("short".to_string()), "0.0.0.0").unwrap();
    let req = Request::builder()
        .uri("/")
        .header("Authorization", "Bearer this-is-much-longer-than-short")
        .body(axum::body::Body::empty())
        .unwrap();
    assert!(!config.check(&req));
}

#[test]
fn test_auth_check_local_addr_without_token() {
    // Local addresses should be allowed without token
    for addr in ["127.0.0.1", "::1", "localhost"] {
        let config = AuthConfig::new(None, addr);
        assert!(config.is_ok(), "addr={}", addr);
    }
}

#[test]
fn test_auth_check_valid_query_token_on_sse_paths() {
    let config = AuthConfig::new(Some("secret123".to_string()), "0.0.0.0").unwrap();
    // The middleware runs inside the `/api/v1` nest, so axum strips the
    // nest prefix and the SSE streams are seen as `/logs` and `/events`.
    for path in [
        "/logs?token=secret123",
        "/logs/?token=secret123",
        "/events?token=secret123",
        "/events/?token=secret123",
    ] {
        let req = Request::builder().uri(path).body(axum::body::Body::empty()).unwrap();
        assert!(config.check(&req), "query token must pass on {path}");
    }
}

#[test]
fn test_auth_check_wrong_query_token_on_sse_logs_path() {
    let config = AuthConfig::new(Some("secret123".to_string()), "0.0.0.0").unwrap();
    let req = Request::builder()
        .uri("/logs?token=wrong-token")
        .body(axum::body::Body::empty())
        .unwrap();
    assert!(!config.check(&req));
}

#[test]
fn test_auth_check_empty_query_token_on_sse_logs_path() {
    let config = AuthConfig::new(Some("secret123".to_string()), "0.0.0.0").unwrap();
    for path in ["/logs?token=", "/logs?token"] {
        let req = Request::builder().uri(path).body(axum::body::Body::empty()).unwrap();
        assert!(!config.check(&req), "empty query token must not pass on {path}");
    }
}

#[test]
fn test_auth_check_query_token_rejected_off_sse_path() {
    let config = AuthConfig::new(Some("secret123".to_string()), "0.0.0.0").unwrap();
    for path in [
        // Even a correct token in the query must NOT authenticate anywhere
        // outside the SSE stream (token in URL leaks into logs/history).
        "/system/version?token=secret123",
        "/config?token=secret123",
        "/logs/download?token=secret123",
    ] {
        let req = Request::builder().uri(path).body(axum::body::Body::empty()).unwrap();
        assert!(!config.check(&req), "query token must NOT pass on {path}");
    }
}

#[test]
fn test_auth_check_query_token_works_alongside_other_query_params() {
    let config = AuthConfig::new(Some("secret123".to_string()), "0.0.0.0").unwrap();
    let req = Request::builder()
        .uri("/logs?stream=json&token=secret123")
        .body(axum::body::Body::empty())
        .unwrap();
    assert!(config.check(&req));
}

// ─── token digest & runtime update ──────────────────────────────

#[test]
fn test_token_digest_never_plaintext() {
    let config = AuthConfig::new(Some("super-secret".to_string()), "0.0.0.0").unwrap();
    let digest = config.token_hash.read().unwrap().clone().expect("token configured");
    assert_ne!(digest, "super-secret", "raw token must never be retained");
    assert_eq!(digest, sha256_hex("super-secret"));
    assert_eq!(digest.len(), 64); // 32 bytes as hex
}

#[test]
fn test_runtime_update_token_swaps_effective_token() {
    let config = AuthConfig::new(Some("old-token".to_string()), "0.0.0.0").unwrap();
    let req = |tok: &str| {
        Request::builder()
            .uri("/system/version")
            .header("Authorization", format!("Bearer {tok}"))
            .body(axum::body::Body::empty())
            .unwrap()
    };
    assert!(config.check(&req("old-token")));
    assert!(!config.check(&req("new-token")));

    config.update_token("new-token").unwrap();
    assert!(!config.check(&req("old-token")), "old token must stop working");
    assert!(
        config.check(&req("new-token")),
        "new token must take effect immediately"
    );
}

#[test]
fn test_update_token_rejects_empty() {
    let config = AuthConfig::new(None, "127.0.0.1").unwrap();
    assert!(config.update_token("   ").is_err());
    assert!(!config.is_enabled());
}

// ─── resolve precedence: env/CLI > auth file > none ─────────────

#[test]
fn test_resolve_explicit_token_beats_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("auth.toml");
    save_auth_file(&store, &sha256_hex("file-token")).unwrap();

    let config = AuthConfig::resolve(Some("env-token".to_string()), "0.0.0.0", Some(store), None).unwrap();
    assert!(config.is_enabled());
    let req = |tok: &str| {
        Request::builder()
            .uri("/")
            .header("Authorization", format!("Bearer {tok}"))
            .body(axum::body::Body::empty())
            .unwrap()
    };
    assert!(config.check(&req("env-token")), "explicit token must win");
    assert!(!config.check(&req("file-token")), "file token must be ignored");
}

#[test]
fn test_resolve_falls_back_to_file_token() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("auth.toml");
    save_auth_file(&store, &sha256_hex("file-token")).unwrap();

    let config = AuthConfig::resolve(None, "0.0.0.0", Some(store), None).unwrap();
    assert!(config.is_enabled(), "no explicit token must fall back to the file");
    let req = Request::builder()
        .uri("/")
        .header("Authorization", "Bearer file-token")
        .body(axum::body::Body::empty())
        .unwrap();
    assert!(config.check(&req));
}

#[test]
fn test_resolve_missing_file_yields_bootstrap_on_loopback() {
    // store_path is a fresh temp file (not the developer's real auth file)
    // so the test is hermetic: a missing file must mean "no token".
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("auth.toml");
    let config = AuthConfig::resolve(None, "127.0.0.1", Some(store), None).unwrap();
    assert!(!config.is_enabled());
    assert!(config.bootstrap_mode());
    assert!(!config.bootstrap_key_required(), "loopback bootstrap needs no key");
}

#[test]
fn test_resolve_non_loopback_no_token_refused_without_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("auth.toml");
    assert!(AuthConfig::resolve(None, "0.0.0.0", Some(store), None).is_err());
}

#[test]
fn test_resolve_non_loopback_bootstrap_key_enters_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("auth.toml");
    let config = AuthConfig::resolve(None, "0.0.0.0", Some(store), Some("boot-key".to_string())).unwrap();
    assert!(!config.is_enabled());
    assert!(config.bootstrap_key_required());
}

// ─── auth file persistence ──────────────────────────────────────

#[test]
fn test_auth_file_round_trip_never_stores_plaintext() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("auth.toml");
    save_auth_file(&store, &sha256_hex("my-secret")).unwrap();

    let content = std::fs::read_to_string(&store).unwrap();
    assert!(
        !content.contains("my-secret"),
        "auth file must not contain the raw token"
    );
    assert!(content.contains("api_token_sha256"));

    assert_eq!(
        load_auth_file(&store).unwrap().unwrap(),
        sha256_hex("my-secret"),
        "load must return the persisted digest"
    );
    assert_eq!(
        load_auth_file(&dir.path().join("missing.toml")).unwrap(),
        None,
        "missing auth file means no token"
    );
}

#[test]
fn test_load_corrupt_auth_file_is_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("auth.toml");
    std::fs::write(&store, "not [ valid toml").unwrap();
    assert!(load_auth_file(&store).is_err());
}

#[test]
fn test_update_token_persists_and_reloads() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("auth.toml");
    let config = AuthConfig::resolve(None, "127.0.0.1", Some(store.clone()), None).unwrap();
    config.update_token("ui-chosen-token").unwrap();

    let content = std::fs::read_to_string(&store).unwrap();
    assert!(!content.contains("ui-chosen-token"), "raw token must not be persisted");
    assert_eq!(load_auth_file(&store).unwrap().unwrap(), sha256_hex("ui-chosen-token"));

    // A fresh config reading the same file starts enabled with that token.
    let reloaded = AuthConfig::resolve(None, "0.0.0.0", Some(store), None).unwrap();
    assert!(reloaded.is_enabled());
    let req = Request::builder()
        .uri("/")
        .header("Authorization", "Bearer ui-chosen-token")
        .body(axum::body::Body::empty())
        .unwrap();
    assert!(reloaded.check(&req));
}
