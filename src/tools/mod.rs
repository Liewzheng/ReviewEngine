//! CLI-style tool commands that can be invoked as part of a review pipeline.
//!
//! Each submodule defines a standalone tool (`ask`, `describe`, `improve`,
//! `repo_review`, `update_changelog`) that encapsulates a specific review
//! operation. These tools are typically driven from the CLI or the server
//! dispatcher and produce structured output such as findings, descriptions,
//! improvement suggestions, or changelog entries. The module serves as a
//! registry of all available tool implementations.

pub mod ask;
pub mod describe;
pub mod improve;
pub mod init;
pub mod repo_review;
pub mod update_changelog;
