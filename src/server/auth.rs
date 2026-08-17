//! API authentication middleware. Validates Bearer tokens for REST API endpoints.
//!
//! @module review-engine: CodeReview Board platform
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::{
    extract::Request,
    http::{Method, StatusCode},
    middleware::Next,
    response::IntoResponse,
    Json,
};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};

/// Generate a random API token (32 hex chars).
pub fn generate_token() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    format!("review_{}", hex::encode(bytes))
}

/// SHA-256 hex digest of a token. The API token is never stored or retained in
/// plaintext: at rest (the auth file) and in memory we keep only this digest,
/// and [`AuthConfig::check`] compares digests in constant time.
fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

fn constant_time_eq_str(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    subtle::ConstantTimeEq::ct_eq(a, b).into()
}

fn is_loopback_bind(bind_addr: &str) -> bool {
    matches!(bind_addr, "127.0.0.1" | "::1" | "localhost")
}

/// Auth configuration for the API server.
///
/// The token is held as its SHA-256 digest (never the raw secret) and wrapped
/// in an `RwLock` so `PUT /api/v1/system/token` can swap it at runtime without
/// a restart. When no token is configured the server runs in *bootstrap mode*:
/// every `/api/v1` endpoint returns `401 {"code":"auth_required"}` except the
/// bootstrap endpoints, letting the first-run UI set the initial token.
#[derive(Debug, Default)]
pub struct AuthConfig {
    /// SHA-256 hex digest of the effective API token; `None` = no token yet.
    token_hash: RwLock<Option<String>>,
    /// SHA-256 hex digest of the explicit env/CLI token (`REVIEW_API_TOKEN` /
    /// `--api-token`) when one was supplied at startup. Kept even after a
    /// runtime rotation so the operator's env-configured token remains a valid
    /// rotation credential — the env-precedence self-rescue path when the
    /// browser holds a stale persisted token.
    explicit_token_hash: Option<String>,
    /// Where `update_token` persists the digest (default
    /// `~/.config/review-engine/auth.toml`, overridable with `REVIEW_AUTH_FILE`).
    /// `None` keeps the token in memory only (tests / degraded environments).
    store_path: Option<PathBuf>,
    /// One-time bootstrap key required to set the FIRST token on a non-loopback
    /// bind. `None` on loopback binds, where bootstrap is open to local callers.
    bootstrap_key: Option<String>,
    /// True when the server binds only loopback — bootstrap needs no key.
    loopback_bind: bool,
}

impl AuthConfig {
    /// Pure constructor (no file I/O). Keeps the legacy semantics — a
    /// non-loopback bind without a token is refused — and is the compatibility
    /// entry point used by tests. The server itself uses [`Self::resolve`].
    pub fn new(token: Option<String>, bind_addr: &str) -> anyhow::Result<Self> {
        let loopback_bind = is_loopback_bind(bind_addr);
        if !loopback_bind && token.is_none() {
            return Err(anyhow::anyhow!(
                "Binding to '{bind_addr}' requires an API token. \
                 Use --api-token <token> or set REVIEW_API_TOKEN. \
                 For local-only access, bind to 127.0.0.1 (default)."
            ));
        }
        let token_hash = token.map(|t| sha256_hex(&t));
        Ok(Self {
            token_hash: RwLock::new(token_hash.clone()),
            explicit_token_hash: token_hash,
            store_path: None,
            bootstrap_key: None,
            loopback_bind,
        })
    }

    /// Resolve the effective token at startup, in precedence order:
    /// explicit (CLI `--api-token` / `REVIEW_API_TOKEN`) > persisted auth file >
    /// none. On a loopback bind, "none" enters first-run bootstrap mode (the
    /// API stays locked behind `401 auth_required` until the initial token is
    /// set). On a non-loopback bind, "none" is refused unless a one-time
    /// `bootstrap_key` is supplied — preserving the legacy guarantee that a
    /// public bind never silently runs without a credential.
    pub fn resolve(
        explicit_token: Option<String>,
        bind_addr: &str,
        store_path: Option<PathBuf>,
        bootstrap_key: Option<String>,
    ) -> anyhow::Result<Self> {
        let loopback_bind = is_loopback_bind(bind_addr);
        let store_path = store_path.or_else(default_auth_file_path);
        let bootstrap_key = bootstrap_key.filter(|k| !k.is_empty());

        let explicit_token_hash = explicit_token.map(|t| sha256_hex(&t));
        let mut token_hash = explicit_token_hash.clone();
        if token_hash.is_none() {
            if let Some(path) = &store_path {
                if let Some(hash) = load_auth_file(path)? {
                    token_hash = Some(hash);
                }
            }
        }

        if token_hash.is_none() && !loopback_bind && bootstrap_key.is_none() {
            return Err(anyhow::anyhow!(
                "Binding to '{bind_addr}' requires an API token or a one-time bootstrap key. \
                 Use --api-token <token> / REVIEW_API_TOKEN, or pass --bootstrap-key <key> / \
                 REVIEW_BOOTSTRAP_KEY for first-run setup (the initial token is then set via the web UI). \
                 For local-only access, bind to 127.0.0.1 (default)."
            ));
        }

        Ok(Self {
            token_hash: RwLock::new(token_hash),
            explicit_token_hash,
            store_path,
            bootstrap_key,
            loopback_bind,
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.token_hash.read().unwrap().is_some()
    }

    /// True while no token is configured — the first-run bootstrap window.
    pub fn bootstrap_mode(&self) -> bool {
        !self.is_enabled()
    }

    /// True when bootstrap requires the one-time key (non-loopback bind).
    pub fn bootstrap_key_required(&self) -> bool {
        self.bootstrap_mode() && !self.loopback_bind
    }

    /// Swap the effective token at runtime and persist its digest to the auth
    /// file. The file is written first; only a successful persist commits the
    /// in-memory value, so a failed write never leaves the server running with
    /// a token that would silently vanish on the next restart.
    pub fn update_token(&self, raw: &str) -> anyhow::Result<()> {
        let raw = raw.trim();
        if raw.is_empty() {
            anyhow::bail!("API token must not be empty");
        }
        let hash = sha256_hex(raw);
        if let Some(path) = &self.store_path {
            save_auth_file(path, &hash)?;
        }
        *self.token_hash.write().unwrap() = Some(hash);
        Ok(())
    }

    pub fn check(&self, req: &Request) -> bool {
        let Some(expected) = self.token_hash.read().unwrap().clone() else {
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
        if provided.is_empty() {
            return false;
        }
        constant_time_eq_str(&sha256_hex(provided), &expected)
    }

    /// Whether `req` carries the explicit env/CLI token (`REVIEW_API_TOKEN` /
    /// `--api-token`). Env-precedence override: even after `update_token`
    /// swapped the effective in-memory token, the operator's env-configured
    /// token stays valid as a rotation credential — a browser holding a stale
    /// persisted token can still rescue rotation with the env token.
    fn check_explicit_token(&self, req: &Request) -> bool {
        let Some(expected) = &self.explicit_token_hash else {
            return false;
        };
        let provided = req
            .headers()
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .or_else(|| req.headers().get("X-API-Key").and_then(|v| v.to_str().ok()))
            .unwrap_or("");
        if provided.is_empty() {
            return false;
        }
        constant_time_eq_str(&sha256_hex(provided), expected)
    }

    /// Whether `req` carries the one-time bootstrap key (first-run on a
    /// non-loopback bind).
    fn valid_bootstrap_key(&self, req: &Request) -> bool {
        let Some(expected) = &self.bootstrap_key else {
            return false;
        };
        let provided = req
            .headers()
            .get("X-Bootstrap-Key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        constant_time_eq_str(provided, expected)
    }
}

/// Schema of the on-disk auth file (`~/.config/review-engine/auth.toml`).
///
/// Only the SHA-256 digest of the API token is stored — never the raw secret —
/// so a backup or accidental copy of the config dir cannot hand out the token.
#[derive(Debug, Serialize, Deserialize)]
struct AuthFile {
    #[serde(default, rename = "api_token_sha256")]
    api_token_sha256: Option<String>,
}

/// Auth file location: `REVIEW_AUTH_FILE` or the default config path.
fn default_auth_file_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("REVIEW_AUTH_FILE") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    home::home_dir().map(|dir| dir.join(".config").join("review-engine").join("auth.toml"))
}

/// Load the persisted token digest. A missing file yields `Ok(None)`; a corrupt
/// file is a hard error — it can only appear via tampering or a broken write,
/// and silently ignoring it would leave auth unexpectedly off.
fn load_auth_file(path: &std::path::Path) -> anyhow::Result<Option<String>> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            anyhow::bail!("failed to read auth file {}: {e}", path.display());
        }
    };
    let parsed: AuthFile =
        toml::from_str(&content).map_err(|e| anyhow::anyhow!("failed to parse auth file {}: {e}", path.display()))?;
    Ok(parsed.api_token_sha256.filter(|h| !h.is_empty()))
}

/// Persist the token digest atomically (temp file + rename) with 0600
/// permissions on Unix, so a crash mid-write never leaves a truncated file.
fn save_auth_file(path: &std::path::Path, hash: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string(&AuthFile {
        api_token_sha256: Some(hash.to_string()),
    })
    .map_err(std::io::Error::other)?;
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
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

/// Axum middleware that gates every `/api/v1` endpoint.
///
/// Always mounted. Behaviour depends on the current runtime token:
/// - Token configured → every endpoint requires a valid `Authorization: Bearer`
///   / `X-API-Key`; otherwise `401 {"error":"unauthorized"}`. `PUT
///   /system/token` additionally accepts the one-time bootstrap key
///   (`X-Bootstrap-Key`) or the explicit env/CLI token as rotation credentials
///   — the self-rescue path when the current token is lost or stale. Ordinary
///   endpoints are NOT unlocked by these; the effective token still gates them.
/// - No token (first-run bootstrap) → `GET /system/auth-status` stays open and
///   `PUT /system/token` is reachable from a loopback bind or with the one-time
///   bootstrap key (`X-Bootstrap-Key`); every other endpoint returns `401
///   {"code":"auth_required"}` so the frontend can switch to the bootstrap
///   screen.
///
/// The router stores the shared config in request extensions as `Arc<AuthConfig>`
/// (see [`crate::server::api::routes`]), so it must be read back with the same
/// type — reading `AuthConfig` directly would never match and silently allow
/// every request.
pub async fn auth_middleware(req: Request, next: Next) -> impl IntoResponse {
    let Some(auth) = req.extensions().get::<Arc<AuthConfig>>() else {
        return Ok(next.run(req).await);
    };

    let path = req.uri().path();
    let is_status = path == "/system/auth-status" && req.method() == Method::GET;
    let is_put_token = path == "/system/token" && req.method() == Method::PUT;

    // Deliberately unauthenticated: reveals only whether a token is configured,
    // which the frontend needs before it has any credentials.
    if is_status {
        return Ok(next.run(req).await);
    }

    if !auth.is_enabled() {
        // First-run bootstrap window: only the token-set endpoint is reachable,
        // and only from a loopback bind or with the one-time bootstrap key.
        if is_put_token && (auth.loopback_bind || auth.valid_bootstrap_key(&req)) {
            return Ok(next.run(req).await);
        }
        if is_put_token {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"code": "bootstrap_key_required"})),
            ));
        }
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"code": "auth_required"})),
        ));
    }

    if auth.check(&req) {
        return Ok(next.run(req).await);
    }
    // Rotation rescue (deadlock break): when the current token is lost or
    // rejected — e.g. the browser cached a stale token while the server's
    // effective token comes from REVIEW_API_TOKEN / auth.toml — the operator
    // can still rotate `PUT /system/token` with either the one-time bootstrap
    // key (`X-Bootstrap-Key`, REVIEW_BOOTSTRAP_KEY / --bootstrap-key) or the
    // explicit env/CLI token (REVIEW_API_TOKEN / --api-token). Ordinary
    // endpoints are deliberately NOT unlocked by these; the effective token
    // still gates them.
    if is_put_token && (auth.valid_bootstrap_key(&req) || auth.check_explicit_token(&req)) {
        return Ok(next.run(req).await);
    }
    Err((
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"error": "unauthorized"})),
    ))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod middleware_tests;
