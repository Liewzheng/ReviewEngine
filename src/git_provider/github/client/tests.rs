use super::*;
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn make_client(server: &MockServer) -> Client {
    Client::new_test("test_token", "https://github.com/owner/repo/pull/1", &server.uri()).unwrap()
}

/// Matcher that only matches requests without a `page` query parameter.
struct NoPage;

impl wiremock::Match for NoPage {
    fn matches(&self, request: &wiremock::Request) -> bool {
        !request.url.query_pairs().any(|(k, _)| k == "page")
    }
}

// ─── fetch_pr_info ──────────────────────────────

#[tokio::test]
async fn test_fetch_pr_info() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "number": 1,
            "title": "Test PR",
            "body": "description",
            "head": {"label": "owner:branch", "ref": "feature", "sha": "abc123"},
            "base": {"label": "owner:main", "ref": "main", "sha": "def456"},
            "user": {"id": 100, "login": "testuser"},
            "merge_commit_sha": null,
            "merged": false
        })))
        .mount(&server)
        .await;

    let client = make_client(&server);
    let info = client.fetch_pr_info().await.unwrap();

    assert_eq!(info.project_path, "owner/repo");
    assert_eq!(info.mr_iid, 1);
    assert_eq!(info.title, "Test PR");
    assert_eq!(info.description, "description");
    assert_eq!(info.source_branch, "feature");
    assert_eq!(info.target_branch, "main");
    assert_eq!(info.git_hash, "abc123");
    assert_eq!(info.base_sha, Some("def456".to_string()));
    assert_eq!(info.merge_commit_sha, None);
    assert_eq!(info.pr_author, Some("testuser".to_string()));
    assert_eq!(info.pr_author_id, Some(100));
}

#[tokio::test]
async fn test_fetch_pr_info_401() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/1"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let client = make_client(&server);
    let err = client.fetch_pr_info().await.unwrap_err();
    assert!(err.to_string().contains("401"), "error should mention 401, got: {err}");
}

#[tokio::test]
async fn test_fetch_pr_info_403() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/1"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let client = make_client(&server);
    let err = client.fetch_pr_info().await.unwrap_err();
    assert!(err.to_string().contains("403"), "error should mention 403, got: {err}");
}

// ─── fetch_diff ─────────────────────────────────

#[tokio::test]
async fn test_fetch_diff_ok() {
    let server = MockServer::start().await;
    let diff_text = "diff --git a/src/main.rs b/src/main.rs\nindex abc..def 100644\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,4 @@\n+new line\n old line";

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/1"))
        .and(header("Accept", "application/vnd.github.v3.diff"))
        .respond_with(ResponseTemplate::new(200).set_body_string(diff_text))
        .mount(&server)
        .await;

    let client = make_client(&server);
    let diff = client.fetch_diff().await.unwrap();
    assert_eq!(diff, diff_text);
}

// ─── create_pr_review ───────────────────────────

#[tokio::test]
async fn test_create_pr_review_ok() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/pulls/1/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 42})))
        .mount(&server)
        .await;

    let client = make_client(&server);
    let id = client.create_pr_review("test body").await.unwrap();
    assert_eq!(id, 42);
}

#[tokio::test]
async fn test_create_pr_review_403() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/pulls/1/reviews"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let client = make_client(&server);
    let err = client.create_pr_review("test body").await.unwrap_err();
    assert!(err.to_string().contains("403"), "error should mention 403, got: {err}");
}

// ─── create_review_comment ──────────────────────

#[tokio::test]
async fn test_create_review_comment_ok() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/pulls/1/comments"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;

    let client = make_client(&server);
    let result = client.create_review_comment("src/main.rs", 10, "nice code").await;
    assert!(result.is_ok());
}

// ─── list_review_comments (paginated) ───────────

#[tokio::test]
async fn test_list_review_comments_paginated() {
    let server = MockServer::start().await;
    let base_uri = server.uri();

    let page1 = json!([
        {"id": 1, "body": "comment1", "user": {"id": 200, "login": "botuser"}, "path": "src/main.rs", "line": 10, "pull_request_review_id": 42}
    ]);
    let page2 = json!([
        {"id": 2, "body": "comment2", "user": {"id": 200, "login": "botuser"}, "path": "src/lib.rs", "line": 20, "pull_request_review_id": 43}
    ]);

    let next_url = format!("{base_uri}/repos/owner/repo/pulls/1/comments?per_page=100&page=2");
    let link_header = format!(r#"<{next_url}>; rel="next", <{next_url}>; rel="last""#);

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/1/comments"))
        .and(query_param("per_page", "100"))
        .and(NoPage)
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page1)
                .insert_header("Link", link_header),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/1/comments"))
        .and(query_param("per_page", "100"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page2))
        .mount(&server)
        .await;

    let client = make_client(&server);
    let comments = client.list_review_comments().await.unwrap();

    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0].id, 1);
    assert_eq!(comments[1].id, 2);
}

// ─── list_pr_reviews (paginated) ────────────────

#[tokio::test]
async fn test_list_pr_reviews_paginated() {
    let server = MockServer::start().await;
    let base_uri = server.uri();

    let page1 = json!([
        {"id": 42, "body": "review body", "user": {"id": 200, "login": "botuser"}, "state": "COMMENT"}
    ]);
    let page2 = json!([
        {"id": 43, "body": "second review", "user": {"id": 200, "login": "botuser"}, "state": "APPROVE"}
    ]);

    let next_url = format!("{base_uri}/repos/owner/repo/pulls/1/reviews?per_page=100&page=2");
    let link_header = format!(r#"<{next_url}>; rel="next", <{next_url}>; rel="last""#);

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/1/reviews"))
        .and(query_param("per_page", "100"))
        .and(NoPage)
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page1)
                .insert_header("Link", link_header),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/1/reviews"))
        .and(query_param("per_page", "100"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page2))
        .mount(&server)
        .await;

    let client = make_client(&server);
    let reviews = client.list_pr_reviews().await.unwrap();

    assert_eq!(reviews.len(), 2);
    assert_eq!(reviews[0].id, 42);
    assert_eq!(reviews[1].id, 43);
}

// ─── get_current_user ───────────────────────────

#[tokio::test]
async fn test_get_current_user_ok() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 200, "login": "botuser"})))
        .mount(&server)
        .await;

    let client = make_client(&server);
    let user = client.get_current_user().await.unwrap();
    assert_eq!(user.id, 200);
    assert_eq!(user.login, "botuser");
}

// ─── encode_content_path / parse_code_search_paths ──

#[test]
fn test_encode_content_path() {
    assert_eq!(encode_content_path("src/main.rs"), "src/main.rs");
    assert_eq!(encode_content_path("a b/c#.rs"), "a%20b/c%23.rs");
}

#[test]
fn test_parse_code_search_paths_dedups_and_caps() {
    let value = json!({
        "total_count": 3,
        "items": [
            {"path": "src/a.rs"},
            {"path": "src/b.rs"},
            {"path": "src/a.rs"},
            {"path": "src/c.rs"}
        ]
    });
    assert_eq!(
        parse_code_search_paths(&value, 20),
        vec!["src/a.rs".to_string(), "src/b.rs".to_string(), "src/c.rs".to_string()]
    );
    assert_eq!(
        parse_code_search_paths(&value, 2),
        vec!["src/a.rs".to_string(), "src/b.rs".to_string()]
    );
}

#[test]
fn test_parse_code_search_paths_tolerates_garbage() {
    assert!(parse_code_search_paths(&json!({"unexpected": true}), 20).is_empty());
    assert!(parse_code_search_paths(&json!({"items": [{"no_path": 1}]}), 20).is_empty());
}

// ─── fetch_file_raw ───────────────────────────

#[tokio::test]
async fn test_fetch_file_raw_ok() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/contents/src/main.rs"))
        .and(query_param("ref", "main"))
        .and(header("Accept", "application/vnd.github.raw+json"))
        .respond_with(ResponseTemplate::new(200).set_body_string("fn main() {}\n"))
        .mount(&server)
        .await;

    let client = make_client(&server);
    let content = client.fetch_file_raw("src/main.rs", "main").await.unwrap();
    assert_eq!(content, "fn main() {}\n");
}

#[tokio::test]
async fn test_fetch_file_raw_404() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/contents/src/missing.rs"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"message": "Not Found"})))
        .mount(&server)
        .await;

    let client = make_client(&server);
    let err = client.fetch_file_raw("src/missing.rs", "main").await.unwrap_err();
    assert!(err.to_string().contains("404"), "error should mention 404, got: {err}");
}

#[tokio::test]
async fn test_fetch_file_raw_rejects_unsafe_path() {
    let server = MockServer::start().await;
    let client = make_client(&server);
    assert!(client.fetch_file_raw("../secret", "main").await.is_err());
    assert!(client.fetch_file_raw("/etc/passwd", "main").await.is_err());
}

// ─── search_code_paths ────────────────────────

#[tokio::test]
async fn test_search_code_paths_ok() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search/code"))
        .and(query_param("q", "authenticate repo:owner/repo"))
        .and(query_param("per_page", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total_count": 2,
            "items": [
                {"path": "src/auth.rs"},
                {"path": "src/login.rs"}
            ]
        })))
        .mount(&server)
        .await;

    let client = make_client(&server);
    let paths = client.search_code_paths("authenticate", 20).await.unwrap();
    assert_eq!(paths, vec!["src/auth.rs".to_string(), "src/login.rs".to_string()]);
}

#[tokio::test]
async fn test_search_code_paths_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search/code"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let client = make_client(&server);
    let err = client.search_code_paths("x", 20).await.unwrap_err();
    assert!(err.to_string().contains("401"), "error should mention 401, got: {err}");
}
