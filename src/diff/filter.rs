//! Diff file filtering utilities. Determines which files should be excluded from review based on path patterns and binary detection.
//!
//!
//! @module review-engine
use crate::models::DiffFile;

const IGNORED_EXTENSIONS: &[&str] = &[
    ".lock",
    ".sum",
    ".png",
    ".jpg",
    ".jpeg",
    ".gif",
    ".svg",
    ".ico",
    ".webp",
    ".exe",
    ".dll",
    ".so",
    ".dylib",
    ".wasm",
    ".woff",
    ".woff2",
    ".ttf",
    ".eot",
    ".pdf",
    ".doc",
    ".docx",
    ".xls",
    ".xlsx",
    ".zip",
    ".tar",
    ".gz",
    ".bz2",
    ".7z",
    ".rar",
    ".mp3",
    ".mp4",
    ".avi",
    ".mov",
    ".mkv",
    ".pyc",
    ".class",
    ".o",
    ".min.js",
    ".min.css",
    ".map",
    "package-lock.json",
];

const IGNORED_PATHS: &[&str] = &[
    "node_modules/",
    "target/",
    ".git/",
    "vendor/",
    ".venv/",
    "venv/",
    "env/",
    "__pycache__/",
    ".next/",
    ".nuxt/",
    "dist/",
    "build/",
    ".generated/",
    "generated/",
];

pub fn should_ignore(file: &DiffFile) -> bool {
    let path = &file.new_path;

    if IGNORED_PATHS.iter().any(|p| path.contains(p)) {
        return true;
    }

    let path_lower = path.to_ascii_lowercase();
    if IGNORED_EXTENSIONS.iter().any(|ext| path_lower.ends_with(*ext)) {
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

    let (lang, count) = ext_langs
        .iter()
        .copied()
        .map(|(ext, lang)| {
            let count = files.iter().filter(|f| f.path.ends_with(&format!(".{ext}"))).count() as u32;
            (lang, count)
        })
        .max_by_key(|(_, count)| *count)
        .unwrap_or(("Unknown", 0));

    if count > 0 {
        lang.to_string()
    } else {
        "Unknown".to_string()
    }
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

    #[test]
    fn should_ignore_binary_png_file() {
        assert!(should_ignore(&make_file("assets/logo.png")));
    }

    #[test]
    fn should_ignore_binary_exe_file() {
        assert!(should_ignore(&make_file("bin/tool.exe")));
    }

    #[test]
    fn should_ignore_lock_files() {
        assert!(should_ignore(&make_file("Cargo.lock")));
        assert!(should_ignore(&make_file("package-lock.json")));
        assert!(should_ignore(&make_file("subdir/package-lock.json")));
    }

    #[test]
    fn should_ignore_minified_files() {
        assert!(should_ignore(&make_file("bundle.min.js")));
        assert!(should_ignore(&make_file("styles.min.css")));
    }

    #[test]
    fn should_ignore_vendor_and_generated_directories() {
        assert!(should_ignore(&make_file("node_modules/lodash/index.js")));
        assert!(should_ignore(&make_file("vendor/github.com/foo/bar.go")));
        assert!(should_ignore(&make_file(".venv/lib/python3/site.py")));
        assert!(should_ignore(&make_file("dist/bundle.js")));
        assert!(should_ignore(&make_file("build/output.js")));
    }

    #[test]
    fn should_ignore_dot_prefixed_font_extensions() {
        assert!(should_ignore(&make_file("fonts/icon.woff")));
        assert!(should_ignore(&make_file("fonts/icon.woff2")));
        assert!(should_ignore(&make_file("fonts/icon.ttf")));
        assert!(should_ignore(&make_file("fonts/icon.eot")));
    }

    #[test]
    fn should_not_ignore_files_with_embedded_extension_name() {
        // Regression test: paths like "myfilewoff" must not be ignored because
        // the old list contained "woff" without a leading dot.
        assert!(!should_ignore(&make_file("myfilewoff")));
        assert!(!should_ignore(&make_file("myfilewoff2")));
        assert!(!should_ignore(&make_file("myfilettf")));
        assert!(!should_ignore(&make_file("myfileeot")));
        assert!(!should_ignore(&make_file("myfilemap")));
    }

    #[test]
    fn should_ignore_source_maps() {
        assert!(should_ignore(&make_file("dist/bundle.js.map")));
    }

    #[test]
    fn should_ignore_binary_extensions_case_insensitive() {
        // The current implementation is case-sensitive for extensions; keep that
        // behavior unchanged (the existing processor tests assert case-insensitive
        // matching, but the original filter was case-sensitive). This test documents
        // the current contract.
        assert!(should_ignore(&make_file("image.PNG")));
    }

    #[test]
    fn detect_language_picks_majority_language() {
        let files = vec![make_file("src/a.rs"), make_file("src/b.rs"), make_file("src/c.py")];
        assert_eq!(detect_language(&files), "Rust");
    }

    #[test]
    fn detect_language_empty_list_returns_unknown() {
        assert_eq!(detect_language(&[]), "Unknown");
    }
}
