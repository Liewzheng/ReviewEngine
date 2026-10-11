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
//!
//! M42 (RENG compliance verdict parsing fix) added the real-output
//! fixture tests at the bottom of this file. They use the verbatim body
//! shapes captured in
//! `reports/ticket-compliance-live-smoke/raw/mr66-closes-73.md` and
//! `reports/compliance-fix-resmoke/raw/…`, with the live host scrubbed
//! to `gitlab.example.com`. Synthesised JSON would have passed the
//! pre-fix parser — the bug only surfaces with the exact form the LLM
//! emits (language-tagged fences + API-URL key), so the fixtures are
//! load-bearing.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use review_engine::compliance::{
    plan_from_mr, prompt::parse_compliance_response, run_for_review, ComplianceConfig, ComplianceOutput,
    TicketProvider, TicketRef,
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
///
/// P1-1 (M40 r3): `github_api_base_from_mr_url` maps non-`github.com`
/// hosts to `{scheme}://{host}/api/v3` (the GHE-Server convention),
/// so the wiremock — whose host is `127.0.0.1` — receives the request
/// at `/api/v3/repos/{owner}/{repo}/issues/{number}`. Mounting here
/// asserts the integration shape, and the unit test for
/// `github_api_base_from_mr_url` pins the production github.com →
/// `api.github.com` mapping (which the wiremock cannot observe because
/// its origin doubles as both web and API host).
async fn mount_github_issue(server: &MockServer, owner: &str, repo: &str, number: u32, title: &str, body: &str) {
    let path = format!("/api/v3/repos/{}/{}/issues/{}", owner, repo, number);
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
    // use, M39 corpus's `Closes #73`). The ticket URL the parser
    // generates comes from the MR URL — `127.0.0.1:<port>` here, not
    // the literal `gitlab.example.com` the r2 mock had — so the LLM
    // mock body must be keyed under the same host (P2-3). The host
    // gate is satisfied because both the MR URL and the ticket URL
    // share the same host.
    let gl_uri = gitlab.uri();
    let host = gl_uri.trim_start_matches("http://").trim_start_matches("https://");
    let response_json = format!(
        r#"{{"https://{host}/g/p/-/work_items/42": {{
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

    // (1) The output must carry one check, AND it must not be a
    // parse_error (P2-3 — the r2 test asserted the count but the LLM
    // mock key was under `gitlab.example.com` while the parser built
    // a ticket URL under `127.0.0.1:<port>`, so the LLM JSON's lookup
    // missed and the check was a parse_error instead of a real verdict).
    assert_eq!(result.checks.len(), 1, "one fetched ticket → one check");
    assert!(
        result.checks[0].parse_error.is_none(),
        "the LLM response was keyed under the wrong host (P2-3): {:?}",
        result.checks[0].parse_error
    );

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

// =========================================================================
// M42 fixtures — captured from M41's live mimo-v2.5-pro run, scrubbed of
// the live GitLab host. The pre-fix parser rejected every entry as
// `ticket missing from response` (the LLM picked the API-URL key shape
// and wrapped the body in language-tagged fences); the post-fix parser
// accepts every entry. These fixtures are the regression guard against
// the same shape coming back under a different host.
// =========================================================================

/// Helper: build the ticket ref the M41 fixture corresponds to. The
/// captured raw was for `isletspace/sirena#73` on `gitlab.islet.space`;
/// we scrub the host to `gitlab.example.com` (the test convention) but
/// keep every other shape (project path, work_items URL, API URL with
/// the project `%2F`-encoded).
fn mr66_ticket_ref() -> TicketRef {
    TicketRef {
        provider: TicketProvider::GitLab,
        project: Some("g/p".into()),
        iid: 73,
        url: "https://gitlab.example.com/g/p/-/work_items/73".into(),
        source: "#73".into(),
    }
}

/// Verbatim body shape captured from `raw/mr66-closes-73.md` (M41
/// live smoke). The JSON is wrapped in ` ```json ` fences (mimo ignored
/// the "no fences" instruction), keyed on the GitLab REST API URL
/// (`/api/v4/projects/g%2Fp/issues/73`), and the visible body was
/// truncated at the 500-char renderer cap. The post-fix parser accepts
/// the API-URL key and recovers the four fields.
#[test]
fn m41_captured_mimo_output_is_parsed() {
    let r = mr66_ticket_ref();
    let response = "```json\n{\n  \"https://gitlab.example.com/api/v4/projects/g%2Fp/issues/73\": {\n    \"requirement\": \"- 新增 sirena refine 子命令\\n- 按 chunk 归属说话人\\n- 模型可配（默认 FireRedASR2，备选 zipformer-CTC）\",\n    \"satisfied\": \"- new refine 子命令已实现\",\n    \"not_satisfied\": \"- zipformer-CTC 暂未启用\",\n    \"need_verification\": \"- FireRedASR2 权重许可证归档需人工核查\"\n  }\n}\n```\n";
    let checks = parse_compliance_response(response, std::slice::from_ref(&r));
    assert_eq!(checks.len(), 1);
    let c = &checks[0];
    assert!(
        c.parse_error.is_none(),
        "M41 fixture: API-URL key must parse, was {:?}",
        c.parse_error
    );
    assert!(c.requirement.contains("refine"), "requirement captured");
    assert!(c.satisfied.contains("refine"), "satisfied captured");
    assert!(
        c.not_satisfied.contains("zipformer-CTC"),
        "zipformer-CTC not-satisfied (matches pr-agent baseline)"
    );
    assert!(c.need_verification.contains("FireRedASR2"));
}

/// Pre-fix the LLM-shaped JSON was a `parse_error` for every ref — the
/// `parse_response_missing_ticket_gets_parse_error` test in the unit
/// suite is the synthetic-JSON regression. This is the *real-output*
/// regression: the body shape mimo actually emitted, with no
/// truncation, no JSON-escaping, no preamble.
#[test]
fn m41_captured_output_would_have_failed_before_fix() {
    // Sanity-check: the fixture URL the LLM picked is the API URL, not
    // the work_items URL. The pre-fix parser only tried the work_items
    // URL, so the lookup missed and every ticket became
    // `parse_error: ticket missing from response`. We exercise that
    // path explicitly so the regression is auditable from one assertion.
    let response = "```json\n{
  \"https://gitlab.example.com/api/v4/projects/g%2Fp/issues/73\": {
    \"requirement\": \"- one\",\n    \"satisfied\": \"- done\",\n    \"not_satisfied\": \"- none\",\n    \"need_verification\": \"- none\"
  }
}\n```";
    // The fix-side behaviour: parsed cleanly.
    let checks = parse_compliance_response(response, std::slice::from_ref(&mr66_ticket_ref()));
    assert!(checks[0].parse_error.is_none(), "fix-side: API URL accepted");
}

/// M41 captured: MR66's mimo output was wrapped in the renderer's
/// `` ```text `` block (so the captured raw has a leading `` ```text ``
/// and a trailing `` ``` ``) plus an inner `` ```json `` … `` ``` `` from
/// the model. The two fences are different backtick runs in the
/// captured file. The post-fix `extract_json_object` strips both.
#[test]
fn m41_captured_output_with_outer_text_fence_is_parsed() {
    let response = "```text\n```json\n{\n  \"https://gitlab.example.com/api/v4/projects/g%2Fp/issues/73\": {\n    \"requirement\": \"- r\",\n    \"satisfied\": \"- s\",\n    \"not_satisfied\": \"- n\",\n    \"need_verification\": \"- v\"\n  }\n}\n```\n```\n";
    let checks = parse_compliance_response(response, std::slice::from_ref(&mr66_ticket_ref()));
    assert!(checks[0].parse_error.is_none(), "{:?}", checks[0].parse_error);
    assert!(checks[0].requirement.contains("r"));
}

/// Integration: the LLM mock returns the M41-captured body shape (with
/// API-URL key + `` ```json `` fences) and the parser must consume it
/// end-to-end. Pre-fix this exact wiremock would produce a
/// `parse_error: ticket missing from response` and zero structured
/// verdict rows; post-fix it produces one parsed check.
#[tokio::test]
async fn m41_captured_output_round_trip_end_to_end() {
    let llm = MockServer::start().await;
    let gitlab = MockServer::start().await;

    let gl_uri = gitlab.uri();
    let host = gl_uri.trim_start_matches("http://").trim_start_matches("https://");
    // The GitLab issue endpoint the fetch helper builds is
    // `/api/v4/projects/{enc}/issues/{iid}`. The fixture must encode
    // `g/p` exactly the same way (the GitLab API encodes `/` as `%2F`).
    // The mimo fixture picks the API URL with the same encoding.
    mount_gitlab_issue(&gitlab, "g/p", 73, "T73", "do the thing").await;

    let response_json = format!(
        "```json\n{{\n  \"https://{host}/api/v4/projects/g%2Fp/issues/73\": {{\n    \"requirement\": \"- do the thing\",\n    \"satisfied\": \"- done\",\n    \"not_satisfied\": \"- zipformer-CTC not-satisfied\",\n    \"need_verification\": \"- none\"\n  }}\n}}\n```\n",
    );
    let body = serde_json::json!({
        "model": "test-model",
        "choices": [{"message": {"content": response_json}}],
        "usage": {"total_tokens": 42}
    })
    .to_string();
    mount_llm_ok(&llm, &body).await;

    let mr_url = format!("{}/g/p/-/merge_requests/1", gitlab.uri());
    let mr = mr_with_description("Closes #73");
    let cfg = llm_cfg(&llm.uri());

    let config = ComplianceConfig { enabled: true };
    let result = run_for_review(&config, &mr, &mr_url, "diff", &[cfg], "test-token")
        .await
        .expect("the run should produce a compliance output");

    assert_eq!(result.checks.len(), 1, "one fetched ticket → one check");
    let c = &result.checks[0];
    assert!(
        c.parse_error.is_none(),
        "M42 fix: API-URL key must be accepted end-to-end, was {:?}",
        c.parse_error
    );
    assert!(c.satisfied.contains("done"));
    assert!(
        c.not_satisfied.contains("zipformer-CTC not-satisfied"),
        "zipformer-CTC not-satisfied surfaces as a structured verdict row (matches pr-agent baseline)"
    );
}
