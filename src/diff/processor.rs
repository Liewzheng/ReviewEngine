//! Diff processing orchestration.
//!
//! Re-exports functions from focused submodules so existing callers
//! (`crate::diff::processor::*`) continue to work without import changes.

pub use super::context::{apply_token_budget, render_diff_text, truncate_long_lines};
pub use super::filter::{detect_language, should_ignore as should_ignore_file};
pub use super::render::render_file_diff;
pub use super::selection::{compress_deletions, sort_files_by_language_and_size};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::DiffFile;

    fn make_file(path: &str) -> DiffFile {
        DiffFile {
            old_path: path.to_string(),
            new_path: path.to_string(),
            path: path.to_string(),
            status: "modified".to_string(),
            additions: 0,
            deletions: 0,
            hunks: vec![],
        }
    }

    #[test]
    fn test_reexports_still_work() {
        let f = make_file("src/main.rs");
        assert!(!should_ignore_file(&f));

        let f = make_file("node_modules/foo.js");
        assert!(should_ignore_file(&f));
    }

    #[test]
    fn test_should_ignore_binary_extensions_case_insensitive() {
        let f = make_file("image.PNG");
        assert!(should_ignore_file(&f));
    }

    #[test]
    fn test_should_ignore_lockfile_subdir_match() {
        let f = make_file("frontend/package-lock.json");
        assert!(should_ignore_file(&f));
    }

    #[test]
    fn test_should_ignore_generated_directory() {
        let f = make_file("target/debug/main.rs");
        assert!(should_ignore_file(&f));
    }
}
