//! Language profile system for language-aware expert evaluation.
//!
//! Profiles are loaded from the `.code-audit-config.toml` configuration file
//! under the `[languages.profiles.*]` section and provide metadata such as
//! comment syntax, test file patterns, and style tool conventions for each
//! programming language. The [`get_profile`] function looks up a profile by
//! language name (e.g. `"Rust"`, `"Python"`) and falls back to Rust as a
//! safe default when no matching profile is found.

use crate::models::LanguageProfile;

/// Built-in default profiles used when the config file does not define
/// `[languages.profiles.*]` or when a language is missing from it.
pub fn builtin_default(name: &str) -> LanguageProfile {
    match name {
        "Rust" => LanguageProfile {
            name: "Rust".to_string(),
            comment_prefixes: vec!["//".to_string()],
            doc_prefixes: vec!["///".to_string(), "//!".to_string()],
            test_patterns: vec!["_test.rs".to_string(), "/tests/".to_string()],
            style_configs: vec![
                "rustfmt.toml".to_string(),
                ".rustfmt.toml".to_string(),
                "clippy.toml".to_string(),
            ],
            naming_hint: "snake_case for functions, PascalCase for types, modules in snake_case".to_string(),
            error_hint: "Proper use of Result/Option, meaningful error messages via anyhow::Context".to_string(),
        },
        "Python" => LanguageProfile {
            name: "Python".to_string(),
            comment_prefixes: vec!["#".to_string()],
            doc_prefixes: vec!["\"\"\"".to_string(), "'''".to_string()],
            test_patterns: vec!["test_".to_string(), "_test.py".to_string(), "/tests/".to_string()],
            style_configs: vec![
                "ruff.toml".to_string(),
                ".ruff.toml".to_string(),
                "mypy.ini".to_string(),
                ".mypy.ini".to_string(),
            ],
            naming_hint: "snake_case for functions/variables, PascalCase for classes, UPPER_CASE for constants"
                .to_string(),
            error_hint: "Use try/except with specific exception types, avoid bare except".to_string(),
        },
        "C" => LanguageProfile {
            name: "C".to_string(),
            comment_prefixes: vec!["//".to_string()],
            doc_prefixes: vec!["///".to_string(), "/**".to_string()],
            test_patterns: vec!["_test.c".to_string(), "/tests/".to_string()],
            style_configs: vec![".clang-format".to_string()],
            naming_hint: "snake_case for functions, UPPER_CASE for macros, PascalCase for types (typedef)".to_string(),
            error_hint: "Check return values of all functions, use errno properly, validate pointers".to_string(),
        },
        "C++" | "CPlusPlus" => LanguageProfile {
            name: "C++".to_string(),
            comment_prefixes: vec!["//".to_string()],
            doc_prefixes: vec!["///".to_string(), "/**".to_string()],
            test_patterns: vec!["_test.cpp".to_string(), "_test.cc".to_string(), "/tests/".to_string()],
            style_configs: vec![".clang-format".to_string(), ".clang-tidy".to_string()],
            naming_hint: "PascalCase for classes, snake_case for functions, UPPER_CASE for macros".to_string(),
            error_hint: "Use RAII for resource management, exceptions for error handling, or expected<T>".to_string(),
        },
        "Java" => LanguageProfile {
            name: "Java".to_string(),
            comment_prefixes: vec!["//".to_string()],
            doc_prefixes: vec!["/**".to_string(), "///".to_string()],
            test_patterns: vec!["Test.java".to_string(), "/test/".to_string(), "/src/test/".to_string()],
            style_configs: vec!["checkstyle.xml".to_string(), ".editorconfig".to_string()],
            naming_hint: "PascalCase for classes, camelCase for methods, UPPER_CASE for constants".to_string(),
            error_hint: "Use checked exceptions for recoverable errors, Optional for nullable values".to_string(),
        },
        "JavaScript" | "TypeScript" => LanguageProfile {
            name: "JavaScript".to_string(),
            comment_prefixes: vec!["//".to_string()],
            doc_prefixes: vec!["/**".to_string(), "///".to_string()],
            test_patterns: vec![
                ".test.js".to_string(),
                ".spec.js".to_string(),
                ".test.ts".to_string(),
                ".spec.ts".to_string(),
                "__tests__".to_string(),
            ],
            style_configs: vec![
                ".prettierrc".to_string(),
                ".eslintrc".to_string(),
                ".eslintrc.json".to_string(),
            ],
            naming_hint: "camelCase for functions/variables, PascalCase for classes, UPPER_CASE for constants"
                .to_string(),
            error_hint: "Use try/catch for async code, check typeof before runtime operations".to_string(),
        },
        "Go" => LanguageProfile {
            name: "Go".to_string(),
            comment_prefixes: vec!["//".to_string()],
            doc_prefixes: vec!["///".to_string(), "// ".to_string()],
            test_patterns: vec!["_test.go".to_string()],
            style_configs: vec![".golangci.yml".to_string(), "golangci.yml".to_string()],
            naming_hint: "camelCase for unexported, PascalCase for exported, MixedCaps for acronyms".to_string(),
            error_hint: "Return errors explicitly, check err != nil, avoid panics in library code".to_string(),
        },
        _ => LanguageProfile {
            name: name.to_string(),
            comment_prefixes: vec!["//".to_string()],
            doc_prefixes: vec!["///".to_string()],
            test_patterns: vec!["/tests/".to_string()],
            style_configs: vec![".editorconfig".to_string()],
            naming_hint: "Follow the project's existing conventions".to_string(),
            error_hint: "Handle errors explicitly, avoid silent failures".to_string(),
        },
    }
}

/// Look up a language profile, merging built-in defaults with any
/// overrides from the application configuration.
pub fn get_profile(name: &str, config: Option<&crate::models::AppConfig>) -> LanguageProfile {
    let Some(cfg) = config else {
        return builtin_default(name);
    };
    if let Some(overrides) = cfg.languages.profiles.get(name) {
        let base = builtin_default(name);
        LanguageProfile {
            comment_prefixes: if !overrides.comment_prefixes.is_empty() {
                overrides.comment_prefixes.clone()
            } else {
                base.comment_prefixes
            },
            doc_prefixes: if !overrides.doc_prefixes.is_empty() {
                overrides.doc_prefixes.clone()
            } else {
                base.doc_prefixes
            },
            test_patterns: if !overrides.test_patterns.is_empty() {
                overrides.test_patterns.clone()
            } else {
                base.test_patterns
            },
            style_configs: if !overrides.style_configs.is_empty() {
                overrides.style_configs.clone()
            } else {
                base.style_configs
            },
            naming_hint: if !overrides.naming_hint.is_empty() {
                overrides.naming_hint.clone()
            } else {
                base.naming_hint
            },
            error_hint: if !overrides.error_hint.is_empty() {
                overrides.error_hint.clone()
            } else {
                base.error_hint
            },
            ..base
        }
    } else {
        builtin_default(name)
    }
}

/// Return the set of all comment prefixes (inline + doc).
pub fn all_comment_prefixes(profile: &LanguageProfile) -> Vec<String> {
    let mut all = profile.comment_prefixes.clone();
    all.extend(profile.doc_prefixes.clone());
    all.sort();
    all.dedup();
    all
}
