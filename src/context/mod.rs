//! Lightweight project context gathering for lead reviews.

pub mod agents_md;
pub mod file_contents;
pub mod gather;

pub use gather::{gather_project_context, sanitize_user_arg, ProjectContext};
