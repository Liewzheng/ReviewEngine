//! GitLab API client and types.
//!
//! The `client` submodule implements the [`GitProvider`] trait for GitLab
//! merge requests, providing methods to fetch MR metadata, diffs, and
//! repository configuration, as well as posting discussions and inline
//! comments via the GitLab REST API. This is a concrete provider that
//! can be plugged into the review pipeline through the
//! [`crate::git_provider`] abstraction layer.

pub mod client;
