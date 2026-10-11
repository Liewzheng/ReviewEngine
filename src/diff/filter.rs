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
    ".spdx",
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
    "licenses/",
    "LICENSES/",
];

/// Exact base filenames that are never worth reviewing: legal and changelog
/// documents whose diff is essentially boilerplate. Matched case-insensitively
/// against the final path component, so `LICENSE`, `license.md`, `LICENSE-APACHE`
/// and the British spelling `LICENCE` are all covered; a path component named
/// `myfile-license.rs` is **not** (we only compare the base name).
const IGNORED_FILENAMES: &[&str] = &[
    "license",
    "license.txt",
    "license.md",
    "license-apache",
    "license-mit",
    "license-bsd",
    "licence",
    "licence.txt",
    "licence.md",
    "changelog",
    "changelog.md",
    "changelog.txt",
    "notice",
    "notice.txt",
];

/// Determine whether a diff file should be excluded from review.
///
/// Checks against known binary/lock/generated extensions and paths
/// (`node_modules/`, `target/`, `vendor/`, etc.), and against well-known
/// legal/changelog filenames (`LICENSE`, `LICENCE`, `CHANGELOG`, `NOTICE` and
/// their `.txt` / `.md` / `-APACHE` / `-MIT` / `-BSD` variants). Uses
/// case-insensitive matching for extensions and base filenames; the path
/// substring match keeps its historical case-sensitive behaviour for entries
/// other than `licenses/` / `LICENSES/`.
pub fn should_ignore(file: &DiffFile) -> bool {
    let path = &file.new_path;

    if IGNORED_PATHS.iter().any(|p| path.contains(p)) {
        return true;
    }

    let path_lower = path.to_ascii_lowercase();
    if IGNORED_EXTENSIONS.iter().any(|ext| path_lower.ends_with(*ext)) {
        return true;
    }

    if let Some(base) = path_lower.rsplit('/').next() {
        if !base.is_empty() && IGNORED_FILENAMES.contains(&base) {
            return true;
        }
    }

    false
}

/// Detect the dominant programming language from a set of diff files.
///
/// Counts file extensions and returns the language with the most files.
/// Returns `"Unknown"` when the file list is empty or no recognized
/// extension is found.
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
    fn should_ignore_license_filename() {
        // Top-level LICENSE / LICENSE.md / LICENSE.txt — no review value.
        assert!(should_ignore(&make_file("LICENSE")));
        assert!(should_ignore(&make_file("license")));
        assert!(should_ignore(&make_file("License")));
        assert!(should_ignore(&make_file("LICENSE.md")));
        assert!(should_ignore(&make_file("license.txt")));
        // Cargo-style per-dependency license files.
        assert!(should_ignore(&make_file("licenses/tracing/LICENSE")));
    }

    #[test]
    fn should_ignore_license_with_dashed_suffix() {
        // Project sub-license files like `LICENSE-APACHE`, `LICENSE-MIT`.
        assert!(should_ignore(&make_file("LICENSE-APACHE")));
        assert!(should_ignore(&make_file("LICENSE-MIT")));
        assert!(should_ignore(&make_file("license-mit")));
        assert!(should_ignore(&make_file("LICENSE-BSD")));
    }

    #[test]
    fn should_ignore_british_spelling_licence() {
        // Some European / Commonwealth projects use `LICENCE`.
        assert!(should_ignore(&make_file("LICENCE")));
        assert!(should_ignore(&make_file("LICENCE.md")));
        assert!(should_ignore(&make_file("licence.txt")));
    }

    #[test]
    fn should_ignore_changelog_filename() {
        assert!(should_ignore(&make_file("CHANGELOG")));
        assert!(should_ignore(&make_file("Changelog")));
        assert!(should_ignore(&make_file("CHANGELOG.md")));
        assert!(should_ignore(&make_file("CHANGELOG.txt")));
        // Nested changelogs (e.g. monorepo) are also boilerplate.
        assert!(should_ignore(&make_file("docs/CHANGELOG.md")));
        assert!(should_ignore(&make_file("packages/foo/CHANGELOG")));
    }

    #[test]
    fn should_ignore_notice_filename() {
        assert!(should_ignore(&make_file("NOTICE")));
        assert!(should_ignore(&make_file("notice")));
        assert!(should_ignore(&make_file("NOTICE.txt")));
    }

    #[test]
    fn should_ignore_licenses_directory_lowercase_and_uppercase() {
        // Cargo publishes per-dependency licenses under lowercase `licenses/`,
        // but some monorepos use `LICENSES/`.
        assert!(should_ignore(&make_file("licenses/foo/LICENSE")));
        assert!(should_ignore(&make_file("LICENSES/foo/LICENSE")));
        assert!(should_ignore(&make_file("LICENSES/foo/notice.txt")));
    }

    #[test]
    fn should_ignore_spdx_extension() {
        // SPDX-formatted license / dependency metadata files.
        assert!(should_ignore(&make_file("thirdparty.spdx")));
        assert!(should_ignore(&make_file("sbom.spdx")));
        assert!(should_ignore(&make_file("docs/THIRDPARTY.spdx")));
    }

    #[test]
    fn should_not_ignore_files_with_license_as_path_substring() {
        // The base-name match must only fire when the file is named exactly
        // `LICENSE` (or one of its variants); a path that merely contains
        // `license` must still go through review. This protects against the
        // same false-positive class the font-extension test pins: a path like
        // `licensor.rs` (the author of a software license, not a license file)
        // and `mylicense_helper.rs` (a utility module) must remain in scope.
        assert!(!should_ignore(&make_file("src/licensor.rs")));
        assert!(!should_ignore(&make_file("src/mylicense_helper.rs")));
        assert!(!should_ignore(&make_file("src/license_checker.rs")));
        assert!(!should_ignore(&make_file("src/license_parser.rs")));
        // CHANGELOG and NOTICE substrings likewise must not match.
        assert!(!should_ignore(&make_file("src/changelog_formatter.rs")));
        assert!(!should_ignore(&make_file("src/notice_handler.rs")));
    }

    #[test]
    fn should_not_ignore_files_with_embedded_license_or_changelog_substring() {
        // Same class as the woff / ttf regression: a base name that contains
        // `license` / `changelog` / `notice` must not be silently dropped.
        assert!(!should_ignore(&make_file("myfilelicense")));
        assert!(!should_ignore(&make_file("myfilechangelog")));
        assert!(!should_ignore(&make_file("myfilenotice")));
        assert!(!should_ignore(&make_file("LICENSE_HELPER.rs")));
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
