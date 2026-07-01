//! API authentication middleware. Validates Bearer tokens for REST API endpoints.
//!
//! @module review-engine: CodeReview Board platform
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
#[derive(Clone, Default)]
pub struct AuthConfig {
    pub token: Option<String>,
}

impl AuthConfig {
    pub fn new(token: Option<String>, bind_addr: &str) -> Self {
        if bind_addr != "127.0.0.1" && token.is_none() {
            panic!(
                "Binding to '{bind_addr}' requires an API token. \
                 Use --api-token <token> or set REVIEW_API_TOKEN. \
                 For local-only access, bind to 127.0.0.1 (default)."
            );
        }
        Self { token }
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
            .unwrap_or("");

        provided == expected
    }
}

/// Axum middleware that checks Authorization / X-API-Key headers.
pub async fn auth_middleware(req: Request, next: Next) -> impl IntoResponse {
    let auth = req.extensions().get::<AuthConfig>();
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
