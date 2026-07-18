//! Webhook completion callbacks for REST review tasks.
//!
//! When a review submitted via the REST API carries a `webhook` URL, the
//! outcome is POSTed to that URL once the task finishes (success or failure).
//! Delivery is fire-and-forget: callbacks run in a background task with a
//! 10s timeout and failures are only logged, never fail the review task.
//!
//! @module review-engine: part of the CodeReview Board virtual engineering team
use std::time::Duration;

use serde::Serialize;
use uuid::Uuid;

/// Maximum time to wait for the user's webhook endpoint before giving up.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(10);

/// JSON body POSTed to the user's webhook URL when a review task finishes.
#[derive(Debug, Serialize)]
pub struct CallbackPayload {
    pub task_id: Uuid,
    /// `"completed"` or `"failed"`.
    pub status: &'static str,
    /// Short human-readable report summary (present on success).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Error message (present on failure).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Only `http`/`https` URLs are accepted as callback targets.
pub fn is_valid_callback_url(url: &str) -> bool {
    match reqwest::Url::parse(url) {
        Ok(parsed) => matches!(parsed.scheme(), "http" | "https"),
        Err(_) => false,
    }
}

/// Fire-and-forget: POST the task outcome to `webhook` in the background.
///
/// Invalid URLs are ignored (logged, no panic) and delivery failures only
/// produce a `warn` log entry — the review task itself is never affected.
pub fn spawn_callback(
    webhook: Option<String>,
    task_id: Uuid,
    status: &'static str,
    summary: Option<String>,
    error: Option<String>,
) {
    let Some(url) = webhook else { return };
    if !is_valid_callback_url(&url) {
        tracing::warn!("Ignoring invalid review webhook URL for task {task_id}");
        return;
    }
    tokio::spawn(async move {
        if let Err(e) = send_callback(&url, task_id, status, summary, error).await {
            tracing::warn!("Review webhook callback failed for task {task_id}: {e}");
        }
    });
}

/// POST the callback payload once, with a 10s timeout.
async fn send_callback(
    url: &str,
    task_id: Uuid,
    status: &'static str,
    summary: Option<String>,
    error: Option<String>,
) -> anyhow::Result<()> {
    let client = reqwest::Client::builder().timeout(CALLBACK_TIMEOUT).build()?;
    let payload = CallbackPayload {
        task_id,
        status,
        summary,
        error,
    };
    let response = client.post(url).json(&payload).send().await?;
    if !response.status().is_success() {
        anyhow::bail!("webhook endpoint returned HTTP {}", response.status());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn test_valid_callback_urls_accepted() {
        assert!(is_valid_callback_url("http://example.com/hook"));
        assert!(is_valid_callback_url("https://example.com/hook?x=1"));
    }

    #[test]
    fn test_invalid_callback_urls_rejected() {
        assert!(!is_valid_callback_url("ftp://example.com/hook"));
        assert!(!is_valid_callback_url("file:///etc/passwd"));
        assert!(!is_valid_callback_url("not-a-url"));
        assert!(!is_valid_callback_url(""));
    }

    #[tokio::test]
    async fn test_spawn_callback_with_invalid_url_does_not_panic() {
        spawn_callback(
            Some("ftp://example.com/hook".to_string()),
            Uuid::new_v4(),
            "completed",
            Some("summary".to_string()),
            None,
        );
        spawn_callback(None, Uuid::new_v4(), "failed", None, Some("err".to_string()));
    }

    #[tokio::test]
    async fn test_send_callback_posts_payload() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let task_id = Uuid::new_v4();
        let url = format!("{}/hook", server.uri());
        send_callback(&url, task_id, "completed", Some("2 findings".to_string()), None)
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["task_id"], task_id.to_string());
        assert_eq!(body["status"], "completed");
        assert_eq!(body["summary"], "2 findings");
        assert!(body.get("error").is_none());
    }

    #[tokio::test]
    async fn test_send_callback_failure_status_is_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let result = send_callback(&server.uri(), Uuid::new_v4(), "failed", None, Some("boom".to_string())).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_spawn_callback_delivers_in_background() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let task_id = Uuid::new_v4();
        spawn_callback(Some(server.uri()), task_id, "failed", None, Some("timeout".to_string()));

        // The callback runs on a spawned task; poll briefly for delivery.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if server.received_requests().await.unwrap().len() == 1 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "callback was not delivered");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let body: serde_json::Value =
            serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
        assert_eq!(body["task_id"], task_id.to_string());
        assert_eq!(body["status"], "failed");
        assert_eq!(body["error"], "timeout");
    }
}
