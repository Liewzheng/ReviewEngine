//! File path normalization utilities. Converts absolute paths to relative for cross-platform consistency.
//!
//! @module review-engine: CodeReview Board platform
use std::path::Path;

/// Normalize a file path for display output:
/// - If the path is absolute and starts with `repo_root`, strip the prefix.
/// - Strip leading `./` or `.\\` (dot-slash) prefix.
/// - Otherwise return the path as-is.
pub fn normalize_path(path: &str, repo_root: Option<&Path>) -> String {
    let path = path.trim();
    if path.is_empty() {
        return path.to_string();
    }

    // If absolute and we have a repo_root, try to strip it
    if let Some(root) = repo_root {
        let p = std::path::Path::new(path);
        if p.is_absolute() {
            if let Ok(relative) = p.strip_prefix(root) {
                return strip_dot_slash(&relative.to_string_lossy());
            }
        }
    }

    strip_dot_slash(path)
}

/// Strip leading `./` or `.\\` (dot-slash) prefix from a path string.
fn strip_dot_slash(path: &str) -> String {
    let path = if let Some(p) = path.strip_prefix("./") {
        p
    } else if let Some(p) = path.strip_prefix(".\\") {
        p
    } else {
        path
    };
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_relative_path_passthrough() {
        assert_eq!(normalize_path("src/main.rs", None), "src/main.rs");
    }

    #[test]
    fn test_strip_dot_slash() {
        assert_eq!(normalize_path("./src/main.rs", None), "src/main.rs");
    }

    #[test]
    fn test_strip_absolute_with_root() {
        let root = Path::new("/home/user/project");
        assert_eq!(
            normalize_path("/home/user/project/src/main.rs", Some(root)),
            "src/main.rs"
        );
    }

    #[test]
    fn test_absolute_without_root_stays() {
        assert_eq!(normalize_path("/etc/passwd", None), "/etc/passwd");
    }

    #[test]
    fn test_empty_path() {
        assert_eq!(normalize_path("", None), "");
    }

    #[test]
    fn test_strip_dot_slash_from_absolute() {
        let root = Path::new("/repo");
        assert_eq!(normalize_path("/repo/./src/lib.rs", Some(root)), "src/lib.rs");
    }
}
