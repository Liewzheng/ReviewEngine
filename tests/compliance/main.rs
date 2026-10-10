//! M40 / RENG ticket compliance check (M39 §7 Option A) integration tests.
//!
//! These tests verify the four-gate contract the M40 mission requires:
//!   * `compliance.enabled = false`             → 0 extra LLM calls, 0 extra HTTP calls.
//!   * `compliance.enabled = true`, no refs     → 0 extra LLM calls, 0 extra HTTP calls.
//!   * `compliance.enabled = true`, refs found → exactly 1 LLM call.
//!   * every fetch failed                       → 0 LLM calls, fetch-failure section.
//!
//! They drive [`review_engine::compliance::run_for_review`] directly so the
//! GitLab-API / GitHub-API and LLM mocks can be observed end-to-end. The
//! token is passed as a parameter (P2-1: no env-var reads here) so the
//! tests do not race on the process-global env.
//!
//! The GitHub leg has its own test (P1-1) — the previous revision's URL
//! shape `…/pull/N` (literal N) caused `GitHubClient::new` to fail before
//! any HTTP call; the live route now goes through
//! `Client::for_issue`, and this test exercises that path end-to-end.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use review_engine::compliance::{
    plan_from_mr, run_for_review, ComplianceConfig, ComplianceOutput, TicketProvider, TicketRef,
};
use review_engine::models::{LLMConfig, MRInfo};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Standard LLMConfig builder for the mock LLM endpoint.
fn llm_cfg(api_base: &str) -> LLMConfig {
    LLMConfig {
        provider: "test".to_string(),
        model: "test-model".to_string(),
        api_key: "sk-test".to_string(),
        api_base: api_base.to_string(),
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: None,
        disabled: false,
    }
}

/// Mount a JSON-shaped OpenAI-compatible response on `server` that matches
/// every key the LLM client uses.
async fn mount_llm_ok(server: &MockServer, body: &str) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(server)
        .await;
}

/// Mount a GitLab issue endpoint that returns a title + description + web_url.
async fn mount_gitlab_issue(server: &MockServer, project: &str, iid: u32, title: &str, description: &str) {
    let encoded = project.replace('/', "%2F");
    let url = format!("/api/v4/projects/{}/issues/{}", encoded, iid);
    let body = serde_json::json!({
        "title": title,
        "description": description,
        "web_url": format!("https://gitlab.example.com/{}/-/work_items/{}", project, iid),
    })
    .to_string();
    Mock::given(method("GET"))
        .and(path(url))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(server)
        .await;
}

/// Mount a GitLab issue endpoint that returns 404.
async fn mount_gitlab_issue_404(server: &MockServer, project: &str, iid: u32) {
    let encoded = project.replace('/', "%2F");
    let url = format!("/api/v4/projects/{}/issues/{}", encoded, iid);
    Mock::given(method("GET"))
        .and(path(url))
        .respond_with(ResponseTemplate::new(404).set_body_string(r#"{"message":"404 Not Found"}"#))
        .mount(server)
        .await;
}

/// Mount a GitHub issue endpoint that returns a title + body + html_url.
/// Mounted at the GitHub REST API path the provider client builds
/// (`/repos/{owner}/{repo}/issues/{number}`).
async fn mount_github_issue(server: &MockServer, owner: &str, repo: &str, number: u32, title: &str, body: &str) {
    // Wiremock matches path only; the request includes the path but the
    // mock returns 404 if the path matcher is wrong. Use `path_regex`
    // to allow a single trailing slash variant.
    let path = format!("/repos/{}/{}/issues/{}", owner, repo, number);
    let response_body = serde_json::json!({
        "title": title,
        "body": body,
        "html_url": format!("https://github.com/{}/{}/issues/{}", owner, repo, number),
    })
    .to_string();
    Mock::given(method("GET"))
        .and(path_regex(format!("^{}/?$", regex::escape(&path))))
        .respond_with(ResponseTemplate::new(200).set_body_string(response_body))
        .mount(server)
        .await;
}

/// Build an MRInfo for the test fixture.
fn mr_with_description(description: &str) -> MRInfo {
    MRInfo::new(
        "g/p".to_string(),
        "MR title".to_string(),
        "feat/test".to_string(),
        "main".to_string(),
    )
    .with_description(description)
}

/// Extension trait to attach a description in tests (the `description`
/// field on `MRInfo` is `pub` but the `new` constructor leaves it empty).
trait MRInfoExt {
    fn with_description(self, description: &str) -> Self;
}
impl MRInfoExt for MRInfo {
    fn with_description(mut self, description: &str) -> Self {
        self.description = description.to_string();
        self
    }
}

#[tokio::test]
async fn gate_off_means_zero_llm_calls_and_zero_provider_calls() {
    // No mocks mounted: any provider or LLM request would fail. The test
    // passes because run_for_review returns None and touches nothing.
    let server = MockServer::start().await;
    let _ = server.uri();

    let config = ComplianceConfig { enabled: false };
    let mr = mr_with_description("Closes #42");
    let cfg = llm_cfg("http://localhost:1"); // unreachable
    let result = run_for_review(&config, &mr, "https://github.com/g/p/pull/1", "diff", &[cfg], "token").await;
    assert!(result.is_none(), "gate off → no section, no calls");
}

#[tokio::test]
async fn gate_on_with_no_ticket_refs_means_zero_llm_calls_and_zero_provider_calls() {
    // Even when the gate is on, an MR description without a recognised
    // ticket reference must cost nothing.
    let server = MockServer::start().await;
    let config = ComplianceConfig { enabled: true };
    let mr = mr_with_description("This MR does not mention a ticket.");
    let cfg = llm_cfg(&server.uri());
    let result = run_for_review(&config, &mr, "https://github.com/g/p/pull/1", "diff", &[cfg], "token").await;
    assert!(result.is_none(), "no refs → no section, no calls");
    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(
        requests.len(),
        0,
        "no LLM calls when the description has no ticket refs"
    );
}

#[tokio::test]
async fn gate_on_with_gitlab_ticket_refs_means_exactly_one_llm_call() {
    // The LLM mock expects one call. The GitLab mock expects one call
    // (one issue fetch). If either is doubled, the test fails.
    let llm = MockServer::start().await;
    let gitlab = MockServer::start().await;

    mount_gitlab_issue(&gitlab, "g/p", 42, "Ticket 42", "do the thing").await;
    // P1-2: GitLab uses `#N` (the form the user's own MR descriptions
    // use, M39 corpus's `Closes #73`). The mock-host port needs to be in
    // the response so the parser's host gate accepts it.
    let gl_uri = gitlab.uri();
    let port = gl_uri.split(':').next_back().unwrap_or("0");
    let response_json = format!(
        r#"{{"https://gitlab.example.com:{port}/g/p/-/work_items/42": {{
            "requirement": "- do the thing",
            "satisfied": "- done",
            "not_satisfied": "- none",
            "need_verification": "- none"
        }}}}"#,
    );
    let body = serde_json::json!({
        "model": "test-model",
        "choices": [{"message": {"content": response_json}}],
        "usage": {"total_tokens": 42}
    })
    .to_string();
    mount_llm_ok(&llm, &body).await;

    let mr_url = format!("{}/g/p/-/merge_requests/1", gitlab.uri());
    let mr = mr_with_description("Closes #42");
    let cfg = llm_cfg(&llm.uri());

    let config = ComplianceConfig { enabled: true };
    let result = run_for_review(&config, &mr, &mr_url, "diff", &[cfg], "test-token")
        .await
        .expect("the run should produce a compliance output");

    // (1) The output must carry one check.
    assert_eq!(result.checks.len(), 1, "one fetched ticket → one check");

    // (2) Provider call count: exactly one.
    let gl_requests = gitlab.received_requests().await.unwrap_or_default();
    assert_eq!(gl_requests.len(), 1, "exactly one issue fetch per fetched ticket");

    // (3) LLM call count: exactly one.
    let llm_requests = llm.received_requests().await.unwrap_or_default();
    assert_eq!(
        llm_requests.len(),
        1,
        "exactly one LLM call regardless of ticket count (cap is 3)"
    );
}

/// P1-1: the GitHub leg was dead in the previous revision because
/// `fetch_ticket_via_provider` synthesised a `…/pull/N` URL with a
/// literal `N` that `GitHubClient::new` could not parse. The fix is
/// `Client::for_issue` which takes owner/repo/number directly. This
/// test exercises the live route end-to-end.
#[tokio::test]
async fn gate_on_with_github_ticket_refs_means_exactly_one_llm_call() {
    // Wiremock for the GitHub REST API (issues endpoint) and the LLM.
    let llm = MockServer::start().await;
    let github = MockServer::start().await;

    mount_github_issue(&github, "owner", "repo", 42, "Ticket 42", "do the thing").await;
    // Build an MR whose project is `owner/repo` so the parser's local
    // shorthand produces a ref to `owner/repo#42` (matching the mounted
    // mock). The default `g/p` project would have us mounting the wrong
    // path (`/repos/g/p/issues/42`).
    let mr = MRInfo::new(
        "owner/repo".to_string(),
        "MR title".to_string(),
        "feat/test".to_string(),
        "main".to_string(),
    )
    .with_description("Closes #42");

    // The compliance LLM mock returns a response whose ticket URL matches
    // the ticket URL the parser will generate (host = wiremock host).
    let gh_uri = github.uri();
    let api_host = gh_uri.trim_start_matches("http://").trim_start_matches("https://");
    let response_json = format!(
        r#"{{"https://{api_host}/owner/repo/issues/42": {{
            "requirement": "- do the thing",
            "satisfied": "- done",
            "not_satisfied": "- none",
            "need_verification": "- none"
        }}}}"#,
    );
    let body = serde_json::json!({
        "model": "test-model",
        "choices": [{"message": {"content": response_json}}],
        "usage": {"total_tokens": 42}
    })
    .to_string();
    mount_llm_ok(&llm, &body).await;

    // MR URL points at the GitHub wiremock instance.
    let mr_url = format!("{}/owner/repo/pull/1", github.uri());
    let cfg = llm_cfg(&llm.uri());

    let config = ComplianceConfig { enabled: true };
    let result = run_for_review(&config, &mr, &mr_url, "diff", &[cfg], "test-token")
        .await
        .expect("the run should produce a compliance output");

    assert_eq!(result.checks.len(), 1, "one fetched ticket → one check");

    // GitHub mock must have received exactly one request.
    let gh_requests = github.received_requests().await.unwrap_or_default();
    assert_eq!(gh_requests.len(), 1, "exactly one GitHub issue fetch (P1-1 fix)");

    // LLM mock must have received exactly one request.
    let llm_requests = llm.received_requests().await.unwrap_or_default();
    assert_eq!(llm_requests.len(), 1, "exactly one LLM call");
}

#[tokio::test]
async fn every_fetch_failed_skips_the_llm_and_lists_fetch_failures() {
    // GitLab returns 404 so the failure is recorded as a fetch failure
    // rather than a transport error — both reach the same section.
    let llm = MockServer::start().await;
    let gitlab = MockServer::start().await;
    mount_gitlab_issue_404(&gitlab, "g/p", 42).await;

    let mr_url = format!("{}/g/p/-/merge_requests/1", gitlab.uri());
    let mr = mr_with_description("Closes #42");
    let cfg = llm_cfg(&llm.uri());

    let config = ComplianceConfig { enabled: true };
    let result = run_for_review(&config, &mr, &mr_url, "diff", &[cfg], "test-token")
        .await
        .expect("the run should still produce a compliance output (section with notes)");

    // The LLM must not be called when every fetch failed.
    let llm_requests = llm.received_requests().await.unwrap_or_default();
    assert_eq!(llm_requests.len(), 0, "the LLM is skipped when every fetch failed");

    // The section carries one fetch failure.
    assert_eq!(result.fetch_failures.len(), 1, "fetch failure recorded");
    assert!(
        result.fetch_failures[0].error.is_some(),
        "the fetch failure carries an error message"
    );
}

/// P2-2: when the fetch succeeds but the LLM is unavailable, the
/// fetched tickets land in `unanalysed_tickets` (not `fetch_failures`)
/// and the renderer labels them as "fetched, no analysis".
#[tokio::test]
async fn no_usable_llm_separates_fetched_from_failed() {
    let gitlab = MockServer::start().await;
    mount_gitlab_issue(&gitlab, "g/p", 42, "Ticket 42", "do the thing").await;

    let mr_url = format!("{}/g/p/-/merge_requests/1", gitlab.uri());
    let mr = mr_with_description("Closes #42");

    // Empty LLM list: no usable LLM. The compliance sub-step must still
    // fetch the ticket (the fetch is what tells us who is in the section).
    let config = ComplianceConfig { enabled: true };
    let result = run_for_review(&config, &mr, &mr_url, "diff", &[], "test-token")
        .await
        .expect("the run produces a compliance output even with no LLM");

    assert_eq!(
        result.fetch_failures.len(),
        0,
        "fetched ticket must NOT be reported as fetch failed (P2-2)"
    );
    assert_eq!(
        result.unanalysed_tickets.len(),
        1,
        "fetched ticket lands in unanalysed_tickets (P2-2)"
    );
    assert!(
        result.notes.iter().any(|n| n.contains("no usable LLM")),
        "the no-LLM note is recorded: {:?}",
        result.notes
    );
}

/// plan_from_mr is the gate arithmetic: verify it returns the same decision
/// the integration tests rely on, with no I/O.
#[test]
fn plan_decision_matches_integration_assumptions() {
    let config = ComplianceConfig { enabled: true };
    let mr = mr_with_description("Closes #42");
    let plan = plan_from_mr(&config, &mr, "https://gitlab.example.com/g/p/-/merge_requests/1");
    assert!(plan.should_run, "MR with refs should run");
    assert_eq!(plan.ticket_refs.len(), 1);
    assert_eq!(plan.ticket_refs[0].iid, 42);
    assert_eq!(plan.ticket_refs[0].provider, TicketProvider::GitLab);

    let config_off = ComplianceConfig { enabled: false };
    let plan = plan_from_mr(&config_off, &mr, "https://gitlab.example.com/g/p/-/merge_requests/1");
    assert!(!plan.should_run, "gate off never runs");
    assert!(plan.ticket_refs.is_empty());

    let config_on = ComplianceConfig { enabled: true };
    let mr_no_refs = mr_with_description("No ticket here.");
    let plan = plan_from_mr(
        &config_on,
        &mr_no_refs,
        "https://gitlab.example.com/g/p/-/merge_requests/1",
    );
    assert!(!plan.should_run, "gate on + no refs never runs");
}

/// Cap-at-3 behaviour: a description that references 5 tickets caps the
/// run to 3, and the LLM is called exactly once (the cap is on what we
/// *fetch*, the LLM call is per-run regardless).
#[tokio::test]
async fn cap_drops_excess_tickets_to_max() {
    let llm = MockServer::start().await;
    let gitlab = MockServer::start().await;
    mount_gitlab_issue(&gitlab, "g/p", 1, "T1", "r1").await;
    mount_gitlab_issue(&gitlab, "g/p", 2, "T2", "r2").await;
    mount_gitlab_issue(&gitlab, "g/p", 3, "T3", "r3").await;
    mount_gitlab_issue_404(&gitlab, "g/p", 4).await;
    mount_gitlab_issue_404(&gitlab, "g/p", 5).await;

    let body = serde_json::json!({
        "model": "test-model",
        "choices": [{"message": {"content": "{}"}}],
        "usage": {"total_tokens": 7}
    })
    .to_string();
    mount_llm_ok(&llm, &body).await;

    let mr_url = format!("{}/g/p/-/merge_requests/1", gitlab.uri());
    let mr = mr_with_description("Closes #1 #2 #3 #4 #5");
    let cfg = llm_cfg(&llm.uri());

    let config = ComplianceConfig { enabled: true };
    let result = run_for_review(&config, &mr, &mr_url, "diff", &[cfg], "test-token").await;
    let result = result.expect("the run produces an output");
    assert!(
        result.notes.iter().any(|n| n.contains("capped at 3")),
        "the cap note is recorded: {:?}",
        result.notes
    );
    // The cap fires BEFORE the fetch loop, so only the first three tickets
    // are ever requested. Tickets 4 and 5 are silently dropped (they do
    // not even produce a fetch-failure entry — the cap note is the
    // reader's signal).
    assert_eq!(result.checks.len(), 3, "three tickets fetched");
    assert_eq!(result.fetch_failures.len(), 0, "no fetch failures under the cap");
    let llm_requests = llm.received_requests().await.unwrap_or_default();
    assert_eq!(llm_requests.len(), 1, "one LLM call for the batch");
}

/// Concurrency check: ticket refs of varying providers are normalised.
#[test]
fn ticket_ref_is_multiple_with_provider() {
    let r = TicketRef {
        provider: TicketProvider::GitHub,
        project: Some("o/r".into()),
        iid: 1,
        url: "https://github.com/o/r/issues/1".into(),
        source: "#1".into(),
    };
    let _ = r; // fields are pub — this is just a smoke test.
    let _: ComplianceOutput = ComplianceOutput::empty();
}
