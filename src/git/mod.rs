//! Local Git operations for repository browsing and diff extraction.
//!
//! The `local` submodule implements [`LocalGitBrowser`], which wraps
//! a local Git repository to provide diff generation between arbitrary
//! refs, file-content retrieval, and commit-log access. This is the
//! primary backend used by the CLI's `review` subcommand when pointing
//! at a local clone.

pub mod local;
