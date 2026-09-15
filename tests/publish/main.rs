//! RENG-71: a rejected inline note must leave a diagnosable trace.
//!
//! The deployment symptom was a round that posted 2 of 2 candidate notes, both
//! rejected, and logged nothing but `Inline note failed — continuing with the
//! remaining findings` — no status, no response body, no anchor, so a `400
//! position is invalid` was indistinguishable from a 403, a 404 or a broken
//! connection. This drives the real publish path (`publish_review_with_diff` →
//! `GitLabProvider` → wiremock) with one anchor GitLab rejects and one it
//! accepts, and asserts on the *log*: the cause, the rejected `file:line`, the
//! status and a truncated response body.
//!
//! This lives in its own integration binary because it captures `tracing`
//! output through a thread-local subscriber, and the tracing callsite interest
//! cache is process-global: in a shared test binary another thread can cache
//! the publisher's callsites as disabled before the subscriber is installed,
//! after which the events are skipped no matter which subscriber is current.
//! A process that has only this test cannot race itself.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use review_engine::models::{Effort, ExpertReport, Finding, ReviewOutput, Severity};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A `tracing` writer that accumulates the subscriber's output in memory.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = CapturedLogs;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn finding(file: &str, line: u32) -> Finding {
    Finding {
        file: file.to_string(),
        line: Some(line),
        line_end: None,
        severity: Severity::High,
        confidence: 9,
        category: "security".to_string(),
        title: "Anchored finding".to_string(),
        summary: String::new(),
        evidence: String::new(),
        impact: String::new(),
        recommendation: "Propagate the error instead of ignoring it".to_string(),
        effort: Effort::Small,
        expert_name: "security".to_string(),
        expert_role: String::new(),
        agrees_with: Vec::new(),
        references: Vec::new(),
    }
}

fn output_with(findings: Vec<Finding>) -> ReviewOutput {
    ReviewOutput::new(vec![ExpertReport {
        expert_name: "security".to_string(),
        markdown: findings
            .iter()
            .map(|f| format!("### {}\n", f.title))
            .collect::<String>(),
        findings,
        raw_llm_response: String::new(),
        parse_error: None,
        raw_dump_path: None,
        llm_provider: None,
        llm_model: None,
        llm_fp: None,
    }])
}

/// A one-hunk unified diff that changes line 1 of each named file — the line
/// both findings anchor to, so the publisher's diff gate admits them.
fn diff_for(files: &[&str]) -> String {
    files
        .iter()
        .map(|file| {
            format!(
                "diff --git a/{file} b/{file}\n\
                 index 1111111..2222222 100644\n\
                 --- a/{file}\n\
                 +++ b/{file}\n\
                 @@ -1,1 +1,2 @@\n\
                 +changed\n"
            )
        })
        .collect()
}

/// GitLab's position-rejection shape — `400` plus a `{"message": …}` body —
/// padded past the 512-char log budget so the truncation is observable. The
/// real MR !54 rejection was never reachable from here; this reproduces its
/// shape, not its bytes.
fn position_rejection_body() -> String {
    let filler = "x".repeat(600);
    format!(
        r#"{{"message":"400 Bad request - Note {{:position=>[\"new_line is not part of the diff\"]}}","detail":"{filler}END-OF-BODY-MARKER"}}"#
    )
}

/// Mount the GitLab endpoints the publish path touches: the current user, the
/// discussion list (empty → the board is created), MR info (for the inline
/// anchor) and the board note. `bad.rs` is rejected on its discussion POST;
/// `good.rs` is accepted.
async fn mount_gitlab(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/v4/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 1})))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/group%2Fproject/merge_requests/1/discussions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/group%2Fproject/merge_requests/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "title": "t",
            "source_branch": "a",
            "target_branch": "b",
            "author": {"id": 1, "name": "Alice"},
            "diff_refs": {"base_sha": "b1", "start_sha": "s1", "head_sha": "h1"}
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v4/projects/group%2Fproject/merge_requests/1/notes"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 7})))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v4/projects/group%2Fproject/merge_requests/1/discussions"))
        .and(body_string_contains("bad.rs"))
        .respond_with(ResponseTemplate::new(400).set_body_string(position_rejection_body()))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v4/projects/group%2Fproject/merge_requests/1/discussions"))
        .and(body_string_contains("good.rs"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 8})))
        .mount(server)
        .await;
}

fn mr_url(server: &MockServer) -> String {
    format!("{}/group/project/-/merge_requests/1", server.uri())
}

/// Bodies of the POSTed inline discussions, in request order.
async fn discussion_posts(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .expect("request recording enabled")
        .into_iter()
        .filter(|request| {
            request.method.as_str() == "POST"
                && request.url.path() == "/api/v4/projects/group%2Fproject/merge_requests/1/discussions"
        })
        .map(|request| String::from_utf8_lossy(&request.body).to_string())
        .collect()
}

/// The reported symptom, end to end: the first candidate's anchor is rejected
/// with a 400, the note is logged with its cause, and the batch carries on to
/// post the next one — the round-after-the-incident behaviour (1 of 1 posted).
#[tokio::test]
async fn test_rejected_inline_note_logs_its_cause_and_the_batch_continues() {
    let logs = CapturedLogs::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::INFO)
            .finish(),
    );

    let server = MockServer::start().await;
    mount_gitlab(&server).await;

    let files = ["bad.rs", "good.rs"];
    let output = output_with(vec![finding("bad.rs", 1), finding("good.rs", 1)]);
    let diff = diff_for(&files);

    let err = review_engine::publish_review_with_diff("token", &mr_url(&server), &output, Some(&diff))
        .await
        .expect_err("a rejected inline note is reported to the caller");

    // Isolation and accounting are unchanged (RENG-60/63): the failure is
    // counted, the other note is still posted, and a 4xx verdict is never
    // retried — one POST per finding.
    let posts = discussion_posts(&server).await;
    assert_eq!(posts.len(), 2, "one POST per finding, no retry on a 400: {posts:?}");
    assert!(posts[0].contains("bad.rs"), "the rejected anchor is attempted first");
    assert!(posts[1].contains("good.rs"), "the next note is still posted");
    assert_eq!(
        err.to_string(),
        "1 inline note(s) failed to publish (1 posted, 0 rolled up, 0 skipped)",
        "the publish outcome is unchanged"
    );

    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();

    // (a) Exactly one WARN, naming the finding that was rejected.
    assert_eq!(
        text.matches("Inline note failed").count(),
        1,
        "one WARN per failure, no spam: {text}"
    );
    assert!(
        text.contains("finding=bad.rs:1 new_path=bad.rs new_line=1 status=400"),
        "the WARN must name the rejected anchor and the status: {text}"
    );
    assert!(
        !text.contains("finding=good.rs"),
        "the cause must be attributable to the rejected finding only: {text}"
    );

    // (b) The GitLab verdict is readable, and its body is truncated.
    assert!(
        text.contains("400 Bad request - Note {:position=>["),
        "the WARN must carry GitLab's response body: {text}"
    );
    assert!(
        text.contains('…'),
        "a long response body must be marked as clipped: {text}"
    );
    assert!(
        !text.contains("END-OF-BODY-MARKER"),
        "the response body must be truncated, not logged whole: {text}"
    );

    // The per-pass summary line keeps its RENG-63 byte shape.
    assert!(
        text.contains(
            "Inline notes: 1 posted, 0 rolled up (board only), 0 policy-excluded, 0 anchor-ineligible, \
             0 skipped, 1 failed (2 findings considered)"
        ),
        "the summary line must keep its shape: {text}"
    );
}
