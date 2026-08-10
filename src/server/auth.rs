//! API authentication middleware. Validates Bearer tokens for REST API endpoints.
//!
//! @module review-engine: CodeReview Board platform
use std::sync::Arc;

use axum::{extract::Request, http::StatusCode, middleware::Next, response::IntoResponse, Json};
use rand::rngs::OsRng;
use rand::RngCore;

/// Generate a random API token (32 hex chars).
pub fn generate_token() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    format!("review_{}", hex::encode(bytes))
}

/// Auth configuration for the API server.
#[derive(Clone, Debug, Default)]
pub struct AuthConfig {
    pub token: Option<String>,
}

impl AuthConfig {
    pub fn new(token: Option<String>, bind_addr: &str) -> anyhow::Result<Self> {
        let is_local = bind_addr == "127.0.0.1" || bind_addr == "::1" || bind_addr == "localhost";
        if !is_local && token.is_none() {
            return Err(anyhow::anyhow!(
                "Binding to '{bind_addr}' requires an API token. \
                 Use --api-token <token> or set REVIEW_API_TOKEN. \
                 For local-only access, bind to 127.0.0.1 (default)."
            ));
        }
        Ok(Self { token })
    }

    pub fn is_enabled(&self) -> bool {
        self.token.is_some()
    }

    pub fn check(&self, req: &Request) -> bool {
        let Some(ref expected) = self.token else {
            return true;
        };

        let provided = req
            .headers()
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .or_else(|| req.headers().get("X-API-Key").and_then(|v| v.to_str().ok()))
            .or_else(|| query_token_for_sse(req))
            .unwrap_or("");

        let provided_bytes = provided.as_bytes();
        let expected_bytes = expected.as_bytes();
        if provided_bytes.len() != expected_bytes.len() {
            return false;
        }
        subtle::ConstantTimeEq::ct_eq(provided_bytes, expected_bytes).into()
    }
}

#[cfg(test)]
mod tests {
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
}

/// Paths that may authenticate via `?token=…` in the query string.
///
/// `EventSource` cannot set request headers, so the browser must carry the API
/// token in the URL for SSE streams. A query token lands in access logs and
/// browser history, so the capability is deliberately NOT global — every other
/// endpoint still requires an `Authorization`/`X-API-Key` header. Keep this
/// allowlist exactly at the SSE streams (`/logs`, `/events`); the NDJSON
/// `/logs/download` endpoint is deliberately excluded — plain fetch can send
/// headers, so it gains nothing from the weaker mechanism.
///
/// The middleware is mounted on the `/api/v1` router (`api::routes`), so axum
/// has already stripped the `/api/v1` prefix from `req.uri().path()` — the
/// streams are seen as `/logs` and `/events`.
fn is_sse_query_token_path(path: &str) -> bool {
    matches!(path, "/logs" | "/logs/" | "/events" | "/events/")
}

/// Extract `?token=…` from the query string, but only for SSE stream paths.
/// The token charset is `review_[0-9a-f]{32}` (URL-safe), so no
/// percent-decoding is needed; an encoded or differently-shaped value simply
/// fails to match and the request is rejected.
fn query_token_for_sse(req: &Request) -> Option<&str> {
    if !is_sse_query_token_path(req.uri().path()) {
        return None;
    }
    req.uri().query()?.split('&').find_map(|kv| kv.strip_prefix("token="))
}

/// Axum middleware that checks Authorization / X-API-Key headers.
///
/// The router stores the shared config in request extensions as `Arc<AuthConfig>`
/// (see [`crate::server::api::routes`]), so it must be read back with the same
/// type — reading `AuthConfig` directly would never match and silently allow
/// every request.
pub async fn auth_middleware(req: Request, next: Next) -> impl IntoResponse {
    let auth = req.extensions().get::<Arc<AuthConfig>>();
    match auth {
        Some(config) if config.is_enabled() && !config.check(&req) => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "unauthorized"})),
            ));
        }
        _ => Ok(next.run(req).await),
    }
}
