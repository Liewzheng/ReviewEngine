//! Diff file filtering utilities. Determines which files should be excluded from review based on path patterns and binary detection.
//!
//!
//! @module review-engine
use crate::models::DiffFile;

const IGNORED_EXTENSIONS: &[&str] = &[
    ".lock", ".sum", ".png", ".jpg", ".jpeg", ".gif", ".svg", ".ico", "woff", "woff2", "ttf", "eot", ".min.js",
    ".min.css", "map",
];

const IGNORED_PATHS: &[&str] = &[
    "node_modules/",
    "target/",
    ".git/",
    "vendor/",
    ".venv/",
    "__pycache__/",
    ".next/",
    "dist/",
    "build/",
];

pub fn should_ignore(file: &DiffFile) -> bool {
    if IGNORED_PATHS.iter().any(|p| file.path.contains(p)) {
        return true;
    }
    if IGNORED_EXTENSIONS.iter().any(|ext| file.path.ends_with(*ext)) {
        return true;
    }
    false
}

pub fn detect_language(files: &[DiffFile]) -> String {
    let ext_langs = [
        ("rs", "Rust"),
        ("py", "Python"),
        ("js", "JavaScript"),
        ("ts", "TypeScript"),
        ("go", "Go"),
        ("java", "Java"),
        ("kt", "Kotlin"),
        ("swift", "Swift"),
        ("c", "C"),
        ("h", "C"),
        ("cpp", "C++"),
        ("hpp", "C++"),
        ("cs", "C#"),
        ("rb", "Ruby"),
        ("php", "PHP"),
        ("scala", "Scala"),
    ];

    ext_langs
        .iter()
        .copied()
        .map(|(ext, lang)| {
            let count = files.iter().filter(|f| f.path.ends_with(&format!(".{ext}"))).count() as u32;
            (lang, count)
        })
        .max_by_key(|(_, count)| *count)
        .map(|(lang, _)| lang.to_string())
        .unwrap_or_else(|| "Unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_file(path: &str) -> DiffFile {
        DiffFile {
            old_path: path.to_string(),
            new_path: path.to_string(),
            path: path.to_string(),
            status: "modified".to_string(),
            additions: 1,
            deletions: 0,
            hunks: vec![],
        }
    }

    #[test]
    fn test_ignore_vendor_path() {
        let f = make_file("vendor/foo/lib.rs");
        assert!(should_ignore(&f));
    }

    #[test]
    fn test_ignore_lock_file() {
        let f = make_file("Cargo.lock");
        assert!(should_ignore(&f));
    }

    #[test]
    fn test_not_ignore_source_file() {
        let f = make_file("src/main.rs");
        assert!(!should_ignore(&f));
    }
}
