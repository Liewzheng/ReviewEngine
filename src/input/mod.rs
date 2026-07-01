//! Input resolution for code review sources.
//!
//! Provides [`resolve_browser`] and [`resolve_diff`] functions that
//! examine a [`ReviewInput`] enum value (representing a local repo,
//! GitLab MR, or GitHub PR) and produce the appropriate [`RepoBrowser`]
//! or diff text. Currently the local-git path is fully implemented;
//! remote-GitLab and remote-GitHub paths return errors with instructions
//! to use the `serve` subcommand instead. This module bridges the CLI
//! input layer with the repository-access layer.

use crate::git::local::LocalGitBrowser;
use crate::models::*;
use anyhow::Result;

/// Select and create the appropriate [`RepoBrowser`] based on [`ReviewInput`].
///
/// - `ReviewInput::LocalRepo` → [`LocalGitBrowser`]
/// - `ReviewInput::GitLabMR` / `GitHubPR` → returns error (not yet implemented)
pub fn resolve_browser(input: &ReviewInput) -> Result<Box<dyn RepoBrowser>> {
    match input {
        ReviewInput::LocalRepo { path, .. } => {
            let browser = LocalGitBrowser::new(path);
            Ok(Box::new(browser))
        }
        ReviewInput::GitLabMR { url: _ } => {
            anyhow::bail!("GitLab API browser not yet implemented via CLI");
        }
        ReviewInput::GitHubPR { url: _ } => {
            anyhow::bail!("GitHub API browser not yet implemented via CLI. Use `review-engine serve --github-token <token>` for webhook-triggered reviews.");
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
