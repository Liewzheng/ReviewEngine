use super::*;
use axum::body::Body;
use axum::http::Request;
use axum::middleware;
use axum::routing::{get, put};
use axum::Router;
use std::sync::Arc;
use tower::ServiceExt;

// ─── middleware bootstrap / auth gating ─────────────────────────

/// Drive a request through `auth_middleware` with stub routes, mirroring
/// the production wiring: middleware layer first, `Extension<AuthConfig>`
/// outermost (see `api::routes`).
async fn run_through_middleware(auth: Arc<AuthConfig>, req: Request<axum::body::Body>) -> axum::response::Response {
    let app = Router::new()
        .route("/system/version", get(|| async { "ok" }))
        .route("/system/token", put(|| async { "saved" }))
        .route("/system/auth-status", get(|| async { "status" }))
        .layer(middleware::from_fn(auth_middleware))
        .layer(axum::Extension(auth));
    app.oneshot(req).await.unwrap()
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn test_middleware_loopback_bootstrap_contract() {
    // Hermetic: fresh temp store path, so a developer's real auth file
    // (~/.config/review-engine/auth.toml) can never flip this into a
    // configured-token state.
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("auth.toml");
    let auth = Arc::new(AuthConfig::resolve(None, "127.0.0.1", Some(store), None).unwrap());

    // Ordinary endpoint → 401 auth_required.
    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("GET")
            .uri("/system/version")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(resp).await, serde_json::json!({"code": "auth_required"}));

    // Bootstrap endpoints stay open on loopback.
    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("GET")
            .uri("/system/auth-status")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("PUT")
            .uri("/system/token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_middleware_non_loopback_bootstrap_needs_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("auth.toml");
    let auth = Arc::new(AuthConfig::resolve(None, "0.0.0.0", Some(store), Some("boot-key".to_string())).unwrap());

    // PUT /system/token without the key → 401 bootstrap_key_required.
    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("PUT")
            .uri("/system/token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        body_json(resp).await,
        serde_json::json!({"code": "bootstrap_key_required"})
    );

    // With the key → allowed through to the handler.
    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("PUT")
            .uri("/system/token")
            .header("X-Bootstrap-Key", "boot-key")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Wrong key is still rejected.
    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("PUT")
            .uri("/system/token")
            .header("X-Bootstrap-Key", "wrong-key")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        body_json(resp).await,
        serde_json::json!({"code": "bootstrap_key_required"})
    );
}

#[tokio::test]
async fn test_middleware_configured_token_requires_old_token_for_update() {
    let auth = Arc::new(AuthConfig::resolve(Some("old-token".to_string()), "0.0.0.0", None, None).unwrap());

    // PUT /system/token without auth → 401 unauthorized (old token required).
    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("PUT")
            .uri("/system/token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(resp).await, serde_json::json!({"error": "unauthorized"}));

    // Wrong token → 401.
    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("PUT")
            .uri("/system/token")
            .header("Authorization", "Bearer wrong-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Old token → allowed to rotate.
    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("PUT")
            .uri("/system/token")
            .header("Authorization", "Bearer old-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_middleware_configured_token_can_rotate_with_bootstrap_key() {
    // Deadlock rescue (方案 A): when the current token is invalid/lost, the
    // one-time bootstrap key must still rotate a configured token — it is
    // NOT limited to the first-run bootstrap window.
    let auth = Arc::new(
        AuthConfig::resolve(
            Some("current-token".to_string()),
            "0.0.0.0",
            None,
            Some("boot-key".to_string()),
        )
        .unwrap(),
    );

    // Rotation without any credential → 401.
    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("PUT")
            .uri("/system/token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Rotation with the bootstrap key → 200 even though a token is set.
    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("PUT")
            .uri("/system/token")
            .header("X-Bootstrap-Key", "boot-key")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Wrong bootstrap key → 401.
    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("PUT")
            .uri("/system/token")
            .header("X-Bootstrap-Key", "wrong-key")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_middleware_configured_token_rotates_with_env_token_after_runtime_rotation() {
    // Env precedence override (方案 B): the explicit (REVIEW_API_TOKEN /
    // --api-token) token remains a valid rotation credential even after a
    // runtime UI rotation swapped the in-memory effective token — the
    // operator's env config always wins.
    let auth = Arc::new(AuthConfig::resolve(Some("env-token".to_string()), "0.0.0.0", None, None).unwrap());
    auth.update_token("ui-token").unwrap(); // runtime rotation via the UI

    // The in-memory effective token authenticates rotation…
    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("PUT")
            .uri("/system/token")
            .header("Authorization", "Bearer ui-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // …and so does the env-configured token, even though it is no longer
    // the effective runtime token.
    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("PUT")
            .uri("/system/token")
            .header("Authorization", "Bearer env-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // A random token is still rejected.
    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("PUT")
            .uri("/system/token")
            .header("Authorization", "Bearer unknown-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_middleware_bootstrap_key_does_not_unlock_ordinary_endpoints_when_configured() {
    // Security model preserved: once a token is configured, the bootstrap
    // key is accepted ONLY for token rotation, never for ordinary endpoints.
    let auth = Arc::new(
        AuthConfig::resolve(
            Some("current-token".to_string()),
            "0.0.0.0",
            None,
            Some("boot-key".to_string()),
        )
        .unwrap(),
    );
    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("GET")
            .uri("/system/version")
            .header("X-Bootstrap-Key", "boot-key")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_middleware_configured_token_gates_ordinary_endpoints() {
    let auth = Arc::new(AuthConfig::resolve(Some("secret".to_string()), "0.0.0.0", None, None).unwrap());
    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("GET")
            .uri("/system/version")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(resp).await, serde_json::json!({"error": "unauthorized"}));

    let resp = run_through_middleware(
        auth.clone(),
        Request::builder()
            .method("GET")
            .uri("/system/version")
            .header("Authorization", "Bearer secret")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}
