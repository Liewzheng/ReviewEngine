//! Input resolution for code review sources.
//!
//! Provides [`resolve_browser`] and [`resolve_diff`] functions that
//! examine a [`ReviewInput`] enum value (representing a local repo,
//! GitLab MR, or GitHub PR) and produce the appropriate [`RepoBrowser`]
//! or diff text. The local-git path shells out to `git`; the remote
//! paths wrap the GitLab/GitHub API providers and take their tokens from
//! the `GITLAB_TOKEN` / `GITHUB_TOKEN` environment variables (the same
//! fallback the CLI uses). Diff resolution remains local-only; remote
//! diffs are fetched by the provider clients directly.

use crate::git::local::LocalGitBrowser;
use crate::models::*;
use anyhow::Result;

/// Select and create the appropriate [`RepoBrowser`] based on [`ReviewInput`].
///
/// - `ReviewInput::LocalRepo` → [`LocalGitBrowser`]
/// - `ReviewInput::GitLabMR` → [`crate::git_provider::gitlab::GitLabProvider`]
/// - `ReviewInput::GitHubPR` → [`crate::git_provider::github::GitHubProvider`]
///
/// Remote browsers read their API token from the `GITLAB_TOKEN` /
/// `GITHUB_TOKEN` environment variable; a missing or empty token is an
/// error.
pub fn resolve_browser(input: &ReviewInput) -> Result<Box<dyn RepoBrowser>> {
    match input {
        ReviewInput::LocalRepo { path, .. } => {
            let browser = LocalGitBrowser::new(path);
            Ok(Box::new(browser))
        }
        ReviewInput::GitLabMR { url } => {
            // Validate the URL first so a malformed URL errors regardless of
            // token configuration.
            let token = std::env::var("GITLAB_TOKEN").unwrap_or_default();
            let provider = crate::git_provider::gitlab::GitLabProvider::new(&token, url)?;
            if token.is_empty() {
                anyhow::bail!("GITLAB_TOKEN environment variable is required to browse GitLab repositories");
            }
            Ok(Box::new(provider))
        }
        ReviewInput::GitHubPR { url } => {
            let token = std::env::var("GITHUB_TOKEN").unwrap_or_default();
            let provider = crate::git_provider::github::GitHubProvider::new(&token, url)?;
            if token.is_empty() {
                anyhow::bail!("GITHUB_TOKEN environment variable is required to browse GitHub repositories");
            }
            Ok(Box::new(provider))
        }
    }
}

/// Get the diff from a review input source.
///
/// Delegates to the appropriate backend to produce the diff text.
pub async fn resolve_diff(input: &ReviewInput) -> Result<String> {
    match input {
        ReviewInput::LocalRepo {
            path,
            base_ref,
            head_ref,
            staged,
            since,
            until,
        } => {
            let browser = LocalGitBrowser::new(path);
            let base = base_ref.as_deref().unwrap_or("main");
            let head = head_ref.as_deref();
            let s = since.as_deref();
            let u = until.as_deref();
            browser.get_diff(base, head, *staged, s, u).await
        }
        ReviewInput::GitLabMR { url: _ } => {
            anyhow::bail!("GitLab diff resolution not yet implemented via CLI");
        }
        ReviewInput::GitHubPR { url: _ } => {
            anyhow::bail!("GitHub diff resolution not yet implemented via CLI. Use `review-engine serve --github-token <token>` for webhook-triggered reviews.");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_browser_local() {
        let input = ReviewInput::local("/tmp/test-repo");
        let browser = resolve_browser(&input);
        assert!(browser.is_ok());
    }

    #[test]
    fn test_resolve_browser_gitlab_invalid_url() {
        let input = ReviewInput::GitLabMR {
            url: "not-a-valid-url".to_string(),
        };
        assert!(resolve_browser(&input).is_err());
    }

    #[test]
    fn test_resolve_browser_github_invalid_url() {
        let input = ReviewInput::GitHubPR {
            url: "https://example.com/not/a/pr".to_string(),
        };
        assert!(resolve_browser(&input).is_err());
    }

    #[test]
    fn test_resolve_browser_gitlab_missing_token() {
        let prev = std::env::var("GITLAB_TOKEN").ok();
        std::env::remove_var("GITLAB_TOKEN");
        let input = ReviewInput::GitLabMR {
            url: "https://gitlab.com/group/project/-/merge_requests/1".to_string(),
        };
        let result = resolve_browser(&input);
        if let Some(v) = prev {
            std::env::set_var("GITLAB_TOKEN", v);
        }
        let err = result.err().expect("expected missing-token error");
        assert!(err.to_string().contains("GITLAB_TOKEN"), "unexpected error: {err}");
    }

    #[test]
    fn test_resolve_browser_github_missing_token() {
        let prev = std::env::var("GITHUB_TOKEN").ok();
        std::env::remove_var("GITHUB_TOKEN");
        let input = ReviewInput::GitHubPR {
            url: "https://github.com/owner/repo/pull/1".to_string(),
        };
        let result = resolve_browser(&input);
        if let Some(v) = prev {
            std::env::set_var("GITHUB_TOKEN", v);
        }
        let err = result.err().expect("expected missing-token error");
        assert!(err.to_string().contains("GITHUB_TOKEN"), "unexpected error: {err}");
    }
}
