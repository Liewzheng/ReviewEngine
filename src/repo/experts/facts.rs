//! Deterministic repository facts ("fact grounding").
//!
//! Values here are computed statically from scan entries and injected into
//! LLM prompts by other units, so models cannot contradict observable
//! reality — e.g. reporting "missing type hints" on a fully annotated
//! Python codebase. Everything is cheap, deterministic, and parser-free.
//!
//! Python annotation statistics use line-level heuristics. Known
//! limitations:
//!
//! - A `def` is any trimmed line starting with `def ` or `async def `; a
//!   match inside a string or comment would be counted (rare in practice).
//! - Multi-line signatures are accumulated until a line ending with `:`,
//!   capped at 64 lines as a guard against malformed input.
//! - Parameters are split on top-level commas with bracket/quote tracking;
//!   triple-quoted default values can still confuse the splitter.
//! - `self`/`cls` need no annotation (mypy does not require it either),
//!   and zero-parameter functions count as fully annotated.

use std::fmt::Write as _;
use std::path::Path;

use crate::repo::FileEntry;

/// Deterministic, statically computed facts about a repository.
///
/// Coverage ratios are `None` when no Python `def` was found — the ratio
/// is undefined, not zero. Consumers must not treat "no Python" as "no
/// annotations".
#[derive(Debug, Clone, Default)]
pub struct RepoFacts {
    /// Total number of Python `def` statements analysed (0 when no Python).
    pub python_def_total: usize,
    /// Number of Python defs with a return-type annotation (`->`).
    pub python_return_annotated: usize,
    /// Number of Python defs whose parameters are all type-annotated.
    pub python_fully_param_annotated: usize,
    /// `python_return_annotated / python_def_total` in `[0.0, 1.0]`;
    /// `None` when no Python defs were found.
    pub python_return_annotation_coverage: Option<f64>,
    /// `python_fully_param_annotated / python_def_total` in `[0.0, 1.0]`;
    /// `None` when no Python defs were found.
    pub python_full_param_annotation_coverage: Option<f64>,
    /// CI configuration files detected among the entries (paths as given
    /// in the entries, sorted and deduplicated).
    pub ci_configs: Vec<String>,
    /// Entries matching test naming conventions (see [`is_test_file`]).
    pub test_files: usize,
    /// Non-test, non-binary, non-generated entries.
    pub source_files: usize,
}

impl RepoFacts {
    /// Render the facts as a compact YAML block for prompt injection.
    ///
    /// Field order is stable; coverage ratios are rendered with two
    /// decimals; `python: null` means no Python defs were found;
    /// `ci_configs: []` means no CI configuration was detected.
    pub fn to_prompt_block(&self) -> String {
        let mut out = String::from("repo_facts:\n");
        if let (Some(ret), Some(param)) = (
            self.python_return_annotation_coverage,
            self.python_full_param_annotation_coverage,
        ) {
            let _ = writeln!(out, "  python:");
            let _ = writeln!(out, "    def_total: {}", self.python_def_total);
            let _ = writeln!(out, "    return_annotated: {}", self.python_return_annotated);
            let _ = writeln!(out, "    return_annotation_coverage: {ret:.2}");
            let _ = writeln!(out, "    fully_param_annotated: {}", self.python_fully_param_annotated);
            let _ = writeln!(out, "    full_param_annotation_coverage: {param:.2}");
        } else {
            let _ = writeln!(out, "  python: null");
        }
        if self.ci_configs.is_empty() {
            let _ = writeln!(out, "  ci_configs: []");
        } else {
            let _ = writeln!(out, "  ci_configs:");
            for c in &self.ci_configs {
                let _ = writeln!(out, "    - \"{c}\"");
            }
        }
        let _ = writeln!(out, "  test_files: {}", self.test_files);
        let _ = writeln!(out, "  source_files: {}", self.source_files);
        out
    }
}

/// Compute repository facts from scan entries.
///
/// CI / test / source classification uses entry metadata only; Python
/// annotation statistics additionally read file contents (unreadable
/// files are skipped, matching the other static experts). The CI check
/// matches on entry paths only — the scanner's hidden-file whitelist is
/// deliberately not duplicated here.
pub fn compute(entries: &[FileEntry]) -> RepoFacts {
    let mut facts = RepoFacts::default();
    let mut ci = std::collections::BTreeSet::new();

    for entry in entries {
        if entry.is_binary || entry.is_generated {
            continue;
        }
        if is_ci_config(&entry.path) {
            ci.insert(entry.path.clone());
        }
        let name = Path::new(&entry.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if is_test_file(name, &entry.path) {
            facts.test_files += 1;
        } else {
            facts.source_files += 1;
        }
        if entry.language == "Python" {
            if let Ok(content) = std::fs::read_to_string(&entry.path) {
                let stats = analyse_python_defs(&content);
                facts.python_def_total += stats.total;
                facts.python_return_annotated += stats.return_annotated;
                facts.python_fully_param_annotated += stats.fully_param_annotated;
            }
        }
    }
    facts.ci_configs = ci.into_iter().collect();

    if facts.python_def_total > 0 {
        let total = facts.python_def_total as f64;
        facts.python_return_annotation_coverage = Some(facts.python_return_annotated as f64 / total);
        facts.python_full_param_annotation_coverage = Some(facts.python_fully_param_annotated as f64 / total);
    }
    facts
}

/// Whether a file looks like a test file by naming convention.
///
/// Language-agnostic heuristics, shared with the test-coverage expert and
/// the code-organization large-file statistic so all of them classify
/// exactly the same files:
///   Rust:    `*_test.rs`, `tests/*.rs`, sibling `tests.rs` / `*_tests.rs`
///            (the `#[cfg(test)] mod tests;` split-file convention)
///   Python:  `test_*.py`, `*_test.py`, `tests/*.py`
///   JS/TS:   `*.test.js`, `*.spec.js`, `*.test.ts`, `*.spec.ts`, `__tests__/*`
///   Go:      `*_test.go`
///   Java:    `*Test.java`, `src/test/*`
///   Swift:   `*Tests.swift`, `Tests/*` (SwiftPM)
///
/// The `.test.` / `.spec.` / `_test.` infixes match in ANY extension
/// (e.g. `foo.test.tsx`, `bar.spec.vue`, `baz_test.ts`), not just the
/// per-language examples listed above — the convention is the infix,
/// not the file type.
pub(crate) fn is_test_file(name: &str, path: &str) -> bool {
    name.contains("_test.")
        || name == "tests.rs"
        || name.ends_with("_tests.rs")
        || name.starts_with("test_")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("Test.java")
        || name.ends_with("Tests.swift")
        || path.contains("/tests/")
        || path.contains("/Tests/")
        || path.starts_with("tests/")
        || path.contains("__tests__")
        || path.contains("/test/")
        || path.starts_with("test/")
        || path.contains("/spec/")
}

/// Return inclusive line ranges (1-based, both ends inclusive) of any
/// `#[cfg(test)] mod <name> { ... }` block found in `content`.
///
/// Returns an empty Vec when the source has no inline tests. This is
/// the bridge that lets the security sweep treat an inline
/// `#[cfg(test)] mod tests { ... }` block the same way it treats a
/// dedicated `tests/foo.rs` file: a regex hit inside the block is a
/// fixture, not a real leak. The exact reproducer the predicate must
/// silence is `crates/sirena-cli/src/upload.rs` lines 673 / 741 / 843
/// / 994 — four `password: "sk"` assignments inside the file's
/// `#[cfg(test)] mod tests { ... }` (RENG-114 follow-up).
///
/// Pragmatic containment heuristic, **not** a full Rust parser:
/// - The attribute must be the bare `#[cfg(test)]` on its own line
///   (with optional whitespace). A `#[cfg(all(test, feature = "…"))]`
///   combination is recognised by a future round; today's reproducer
///   shape is the bare form.
/// - Any number of additional `#[…]` attribute lines between
///   `#[cfg(test)]` and the `mod` opener are skipped — the test
///   modules in the wild carry `#[allow(unused)]`, `#[cfg(feature)]`
///   and similar companions.
/// - The `mod` opener must contain a `{` on the same line (the
///   standard convention). Multi-line `mod` declarations (no opening
///   brace on the opener line) fall through to the next `#[cfg(test)]`
///   search, so no production code is mistakenly swallowed.
/// - Brace counting only tallies `{` and `}` that appear **outside**
///   string literals (`"..."`), char literals (`'...'`), line
///   comments (`// …`) and block comments (`/* … */`); see
///   [`count_code_braces`] for the scanner. A `}` inside a string
///   or comment cannot prematurely close the span, and a `{` inside
///   a string or comment cannot over-extend the span past the
///   module's real closing brace. Without the string/comment filter,
///   a single `assert_eq!(brace("{"), 1);` inside a test would push
///   the span past every following production line and silently
///   suppress the security sweep there (false negative — a real
///   leak can be swallowed).
/// - Recognised string / char literal escapes are the standard Rust
///   ones: `\\`, `\"`, `\'`, `\n` and any other `\X` pair is consumed
///   as a two-character escape (the scanner advances by 2, no
///   semantic interpretation). Backslash-newline (line continuation
///   inside a string) **is** tracked across lines via the
///   `in_string` state threaded through successive calls: a
///   `"…\"` at the end of one line keeps the string open on the next
///   line until the matching closing `"`. Multi-line block comments
///   (`/* … */` that cross a newline) are tracked the same way via
///   the `in_block_comment` state.
/// - The single-quote rule: a `'` opens a char literal only when the
///   pattern is `'\…'` (escape) or `'<X>'` (X is exactly one byte
///   then a closing `'`); otherwise the `'` starts a Rust lifetime
///   reference (`&'static str`, `'_`, `<'a>`) and the scanner
///   consumes the `'` + following identifier without entering a
///   literal state. Treating lifetimes as literals would swallow
///   the line's `{` / `}` and bring the RENG-114 false positive
///   back (false-negative direction: production code after the
///   test module loses its security sweep coverage).
/// - Raw string literals `r"…"` / `r#"…"#` and byte / c-string
///   prefixes are recognised: the `#`s after the prefix are
///   consumed and the matching `"<#×N>` is found. Inside a raw
///   string the backslash is a literal character (Rust does not
///   interpret escapes in raw strings), so a `\"` inside
///   `r#"…"#` is two characters and must not close the literal.
///   The reproducer in `src/publisher/mod.rs` line 1171
///   (`r#"{\"message\":…}"#`) has exactly this shape. The only
///   documented limitation is a multi-line raw string that crosses
///   a newline: the hash count is captured on the opening line
///   and the body is scanned for the matching `"<#×N>`; the r2
///   reproducer corpus does not have such a pattern, but if a
///   future reproducer does, the helper can be extended to
///   re-detect the prefix on continuation lines.
///
/// Returns `Vec<(start_line, end_line)>` in 1-based inclusive
/// coordinates so callers can compare against `enumerate() + 1`
/// directly without an off-by-one.
pub(crate) fn cfg_test_line_spans(content: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut i = 0usize;
    while i < lines.len() {
        // Track block comments and unterminated strings even on
        // lines we skip (a `/* ... */` or a backslash-continued
        // `"…\"` can straddle the `#[cfg(test)]` attribute, so the
        // brace-counting state must be current by the time we walk
        // the mod body).
        let (pre_delta, after_bc, after_s) = count_code_braces(lines[i], in_block_comment, in_string);
        in_block_comment = after_bc;
        in_string = after_s;
        let trimmed = lines[i].trim();
        if trimmed != "#[cfg(test)]" || pre_delta != 0 {
            // Not a `#[cfg(test)]` attribute line — the `pre_delta != 0`
            // guard rejects attribute lines that happen to live inside
            // a brace-bearing string/comment fragment of the *outer*
            // block (the reproducer corpus has none, but a corrupted
            // source could); they would otherwise look like the
            // module-closing brace line.
            i += 1;
            continue;
        }
        // Allow additional attribute lines between `#[cfg(test)]` and the `mod`.
        let mut j = i + 1;
        while j < lines.len() && lines[j].trim_start().starts_with('#') {
            j += 1;
        }
        if j >= lines.len() {
            break;
        }
        let mod_trimmed = lines[j].trim();
        if !mod_trimmed.starts_with("mod ") || !mod_trimmed.contains('{') {
            // Not a `mod` opener on the same line — advance past the
            // attribute block so we don't re-match the same `#[cfg(test)]`
            // if a stray line follows.
            i = j + 1;
            continue;
        }
        // Walk from the `mod` line forward, tracking braces until
        // depth returns to 0. The scanner only tallies braces
        // outside strings and comments (see [`count_code_braces`]),
        // so the `}` that closes the mod body is the brace that
        // triggers `break`.
        let mut depth: i32 = 0i32;
        let mut k = j;
        let mut ic = in_block_comment;
        let mut is_ = in_string;
        loop {
            let (delta, after_bc, after_s) = count_code_braces(lines[k], ic, is_);
            ic = after_bc;
            is_ = after_s;
            depth += delta;
            if depth <= 0 {
                break;
            }
            k += 1;
            if k >= lines.len() {
                break;
            }
        }
        in_block_comment = ic;
        in_string = is_;
        // Convert to 1-based inclusive coordinates.
        let start_line = i + 1;
        let end_line = k + 1;
        spans.push((start_line, end_line));
        // Resume scanning past the closing brace.
        i = k + 1;
    }
    spans
}

/// Count the `{` and `}` characters in `line` that appear outside
/// string literals (`"…"`), char literals (`'…'`), line comments
/// (`// …`) and block comments (`/* … */`); return the net depth
/// delta for the line together with the updated
/// `(in_block_comment, in_string)` state for the next call. Both
/// states are threaded across line boundaries — a `/* … */` or a
/// backslash-continued `"…\"` that crosses a newline stays open
/// until the matching closer (`*/` or `"`).
///
/// Special cases (documented so a future reproducer that hits them
/// can extend the helper):
/// - Multi-line strings are handled via the `in_string` state
///   threaded across lines: a `"…\"` at the end of one line keeps
///   the string open on the next line until the matching closing
///   `"`. (Rust's backslash-newline continuation is what makes
///   this necessary; the reproducer in `src/publisher/mod.rs` uses
///   the pattern at lines 1518–1519.)
/// - Rust lifetimes (`&'static str`, `'_`, `<'a>`) are NOT char
///   literals. A `'` opens a char literal only when the pattern is
///   `'\…'` (escape) or `'<X>'` (X is exactly one byte, then a
///   closing `'`); any other shape — `'static`, `'_`, `'a_b_c` —
///   is a lifetime reference, and the scanner consumes the `'` +
///   following identifier and continues. Treating lifetimes as
///   literals swallows the line's `{` / `}` (the reviewer-facts
///   r2 counterexample: `assert_eq!(brace("{"), 1);` inside a test
///   pushed the r2 span past the real close at line 8 and made
///   the security sweep miss the production `api_key` at line 10).
/// - Raw string literals `r"…"` / `r#"…"#` and the byte / c-string
///   prefixes are recognised: the `#`s after the prefix are
///   consumed and the matching `"<#×N>` is found. Inside a raw
///   string the backslash is a literal character (Rust does not
///   interpret escapes in raw strings), so a `\"` inside
///   `r#"…"#` is two characters and must not close the literal.
///   The reproducer in `src/publisher/mod.rs` line 1171 has exactly
///   this shape.
fn count_code_braces(line: &str, mut in_block_comment: bool, mut in_string: bool) -> (i32, bool, bool) {
    let mut depth_delta: i32 = 0;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_block_comment {
            // Inside a /* ... */: look for the matching */.
            if c == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if in_string {
            // Continue scanning inside the still-open string. The
            // only thing we care about is the closing `"`, which
            // is reached when a backslash escape (`\<X>` pair,
            // consumed as two bytes) is not in effect.
            match bytes[i] {
                b'\\' if i + 1 < bytes.len() => {
                    i += 2;
                }
                b'"' => {
                    in_string = false;
                    i += 1;
                }
                _ => i += 1,
            }
            continue;
        }
        match c {
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                // Line comment: ignore the rest of this line.
                break;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                // Block comment opens; ignore braces until the matching */.
                in_block_comment = true;
                i += 2;
            }
            b'"' => {
                // String literal opens. The scanner is now inside
                // a string: set the flag so any brace-like byte
                // until the matching close is not counted, and the
                // state is correctly threaded if the close is
                // past the line's end (`\<newline>` continuation).
                // Scan to the unescaped closing `"`, honouring
                // `\<X>` escape pairs.
                in_string = true;
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' if i + 1 < bytes.len() => {
                            i += 2;
                        }
                        b'"' => {
                            i += 1;
                            in_string = false;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                if i >= bytes.len() && in_string {
                    // Unterminated at EOL — the next call must
                    // continue scanning inside the string.
                    return (depth_delta, in_block_comment, in_string);
                }
            }
            b'\'' => {
                // Either a char literal or a Rust lifetime reference
                // (`&'static str`, `'_`, `<'a>`). Distinguish by what
                // follows the `'`. A char literal is exactly one of:
                //   - `'\X'` — an escape sequence; `\` is the
                //     leading byte, the X is consumed as part of
                //     the escape (the scanner advances by 2)
                //   - `'<X>'` — a single byte X then a closing `'`
                // Anything else — `'<identifier>` (multi-char idents
                // include `_`, letters, digits), `'<no close>` — is
                // a lifetime reference, NOT a literal: consume the `'`
                // and the following identifier chars and continue
                // scanning. Treating lifetimes as char literals
                // would start a bogus scan that swallows the line's
                // remaining `{` / `}`, under-counting opens and
                // making the outer loop's depth-0 break fire at an
                // inner `}` (false-negative direction: production
                // code after the test module loses its security
                // sweep coverage, and the RENG-114 false positive
                // can come back). The reviewer-facts r2
                // counterexample (`assert_eq!(brace("{"), 1);`
                // inside a test) and the audit on the 4 repo files
                // + sirena `upload.rs` both relied on this rule.
                let next = if i + 1 < bytes.len() { Some(bytes[i + 1]) } else { None };
                let is_escape = next == Some(b'\\');
                let is_one_char_literal =
                    !is_escape && next.is_some() && next != Some(b'\'') && i + 2 < bytes.len() && bytes[i + 2] == b'\'';
                if is_escape || is_one_char_literal {
                    // Char literal: scan to the closing `'`,
                    // honouring `\<X>` escape pairs (e.g. `'\n'`,
                    // `'\''`, `'\\'`). The two-byte escape rule
                    // matches the string-literal arm above; any
                    // specific escape sequence is uninterpreted.
                    i += 1;
                    while i < bytes.len() {
                        match bytes[i] {
                            b'\\' if i + 1 < bytes.len() => {
                                i += 2;
                            }
                            b'\'' => {
                                i += 1;
                                break;
                            }
                            _ => i += 1,
                        }
                    }
                } else {
                    // Lifetime or stray `'`: skip the `'` and the
                    // identifier (or empty run) that follows.
                    i += 1;
                    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                        i += 1;
                    }
                }
            }
            b'r' | b'b' | b'c' => {
                // Raw / byte / C-string prefix: `r"…"`, `b"…"`,
                // `c"…"`, and the multi-hash forms `r#"…"#` /
                // `r##"…"##` / `br#"…"#` etc. The `#`s are not
                // brace or quote delimiters, so the prefix
                // detection walks them; once the opening `"` is
                // reached the scanner enters a raw-string state
                // where `\` is a literal character (Rust's raw
                // strings do not interpret escapes — the
                // reproducer in `src/publisher/mod.rs` has
                // `r#"{\"message\":...}"#`, and the in-string `\"`
                // must not consume the following `"` as a 2-byte
                // escape).
                let prefix_start = i;
                let mut p = i + 1;
                if p < bytes.len() && (bytes[p] == b'b' || bytes[p] == b'c') {
                    p += 1;
                } else if p < bytes.len() && bytes[p] == b'r' {
                    p += 1;
                }
                if p < bytes.len() && bytes[p] == b'r' {
                    p += 1;
                }
                while p < bytes.len() && bytes[p] == b'#' {
                    p += 1;
                }
                if p >= bytes.len() || bytes[p] != b'"' {
                    // Bare `r` / `b` / `c` / `br` / `rb` token —
                    // method name, ident, etc. — not a string
                    // prefix; continue the main scan.
                    i = prefix_start + 1;
                    continue;
                }
                let mut hash_count = 0usize;
                let mut q = prefix_start + 1;
                if q < bytes.len() && (bytes[q] == b'b' || bytes[q] == b'c') {
                    q += 1;
                } else if q < bytes.len() && bytes[q] == b'r' {
                    q += 1;
                }
                if q < bytes.len() && bytes[q] == b'r' {
                    q += 1;
                }
                while q < bytes.len() && bytes[q] == b'#' {
                    hash_count += 1;
                    q += 1;
                }
                let body_start = q + 1;
                let mut k = body_start;
                let mut needle = Vec::with_capacity(1 + hash_count);
                needle.push(b'"');
                for _ in 0..hash_count {
                    needle.push(b'#');
                }
                let mut found = false;
                while k + needle.len() <= bytes.len() {
                    if &bytes[k..k + needle.len()] == needle.as_slice() {
                        i = k + needle.len();
                        found = true;
                        break;
                    }
                    k += 1;
                }
                if !found {
                    // Unterminated raw string at EOL — the next
                    // call must continue scanning inside the raw
                    // string. (The reproducer corpus does not
                    // have multi-line raw strings; documented in
                    // case a future reproducer needs the
                    // behaviour.)
                    in_string = true;
                    return (depth_delta, in_block_comment, in_string);
                }
            }
            b'{' => {
                depth_delta += 1;
                i += 1;
            }
            b'}' => {
                depth_delta -= 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    (depth_delta, in_block_comment, in_string)
}

/// Whether `path` is a well-known CI configuration file.
///
/// Matches on file name / directory location only; entries have already
/// passed the scanner's filtering, so no whitelist logic is duplicated.
fn is_ci_config(path: &str) -> bool {
    let name = Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or("");
    let norm = path.replace('\\', "/");
    matches!(
        name,
        ".gitlab-ci.yml"
            | ".gitlab-ci.yaml"
            | "Jenkinsfile"
            | ".travis.yml"
            | "azure-pipelines.yml"
            | "bitbucket-pipelines.yml"
            | "appveyor.yml"
            | ".drone.yml"
    ) || norm.starts_with(".github/workflows/")
        || norm.contains("/.github/workflows/")
        || norm.starts_with(".circleci/")
        || norm.contains("/.circleci/")
        || norm.starts_with(".buildkite/")
        || norm.contains("/.buildkite/")
}

/// Running totals for Python `def` signatures in one file.
#[derive(Default)]
struct DefStats {
    total: usize,
    return_annotated: usize,
    fully_param_annotated: usize,
}

/// Line-level scan for `def` signatures; see the module docs for the
/// heuristic's known limitations.
fn analyse_python_defs(content: &str) -> DefStats {
    let mut stats = DefStats::default();
    let mut sig = String::new();
    let mut in_sig = false;
    let mut sig_lines = 0usize;

    for line in content.lines() {
        let trimmed = line.trim();
        if in_sig {
            sig.push(' ');
            sig.push_str(trimmed);
            sig_lines += 1;
            // A signature ends at the line whose trailing ':' opens the body.
            if trimmed.ends_with(':') || sig_lines >= 64 {
                record_signature(&sig, &mut stats);
                in_sig = false;
                sig.clear();
            }
            continue;
        }
        if trimmed.starts_with("def ") || trimmed.starts_with("async def ") {
            if trimmed.ends_with(':') {
                record_signature(trimmed, &mut stats);
            } else {
                in_sig = true;
                sig.clear();
                sig.push_str(trimmed);
                sig_lines = 1;
            }
        }
    }
    // Unterminated signature at EOF (malformed input): still count it.
    if in_sig {
        record_signature(&sig, &mut stats);
    }
    stats
}

/// Update `stats` with one accumulated signature.
fn record_signature(sig: &str, stats: &mut DefStats) {
    stats.total += 1;

    // Return annotation: `->` between the final `)` and the trailing `:`.
    let body = sig.trim_end();
    let body = body.strip_suffix(':').unwrap_or(body);
    if let Some(close) = body.rfind(')') {
        if body[close + 1..].contains("->") {
            stats.return_annotated += 1;
        }
    }

    // Parameter list: between the first `(` and the final `)`.
    let params = match (sig.find('('), sig.rfind(')')) {
        (Some(open), Some(close)) if open < close => &sig[open + 1..close],
        _ => "",
    };
    let mut all_annotated = true;
    for token in split_top_level_commas(params) {
        let token = token.trim();
        // Bare `*` / `/` are PEP 484/570 separator markers, not parameters.
        if token.is_empty() || token == "*" || token == "/" {
            continue;
        }
        let token = token.trim_start_matches('*').trim();
        if token.is_empty() {
            continue;
        }
        // Drop any default value before checking for the `name: type` colon.
        let head = token.split('=').next().unwrap_or(token).trim();
        if head == "self" || head == "cls" {
            continue;
        }
        if !head.contains(':') {
            all_annotated = false;
        }
    }
    // Zero-parameter functions are vacuously fully annotated.
    if all_annotated {
        stats.fully_param_annotated += 1;
    }
}

/// Split a parameter list on top-level commas, tracking bracket depth and
/// simple quotes. Triple-quoted strings are not tracked (documented limit).
fn split_top_level_commas(params: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut current = String::new();
    for ch in params.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quote.is_some() => {
                current.push(ch);
                escaped = true;
            }
            '\'' | '"' => {
                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(ch);
                }
                current.push(ch);
            }
            '(' | '[' | '{' if quote.is_none() => {
                depth += 1;
                current.push(ch);
            }
            ')' | ']' | '}' if quote.is_none() => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 && quote.is_none() => parts.push(std::mem::take(&mut current)),
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, language: &str) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            language: language.to_string(),
            loc: 10,
            is_binary: false,
            is_generated: false,
        }
    }

    fn write_py(dir: &tempfile::TempDir, name: &str, content: &str) -> String {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&path, content).expect("write python fixture");
        path.to_string_lossy().into_owned()
    }

    /// Two fully annotated defs (one async).
    const ANNOTATED_PY: &str = r#"
def add(a: int, b: int) -> int:
    return a + b


async def fetch(url: str) -> bytes:
    return b""
"#;

    /// One unannotated def, one partially annotated multi-line def.
    const MIXED_PY: &str = r#"
def greet(name):
    return "hi " + name


def multi(
    a: int,
    b,
) -> str:
    return str(a) + b
"#;

    #[test]
    fn python_annotation_coverage_is_computed_per_def() {
        let dir = tempfile::tempdir().expect("tempdir");
        let annotated = write_py(&dir, "annotated.py", ANNOTATED_PY);
        let mixed = write_py(&dir, "mixed.py", MIXED_PY);
        let facts = compute(&[entry(&annotated, "Python"), entry(&mixed, "Python")]);

        assert_eq!(facts.python_def_total, 4);
        assert_eq!(facts.python_return_annotated, 3);
        assert_eq!(facts.python_fully_param_annotated, 2);
        assert_eq!(facts.python_return_annotation_coverage, Some(0.75));
        assert_eq!(facts.python_full_param_annotation_coverage, Some(0.5));
    }

    #[test]
    fn python_coverage_is_none_without_python() {
        let facts = compute(&[entry("src/main.rs", "Rust")]);
        assert_eq!(facts.python_def_total, 0);
        assert_eq!(facts.python_return_annotation_coverage, None);
        assert_eq!(facts.python_full_param_annotation_coverage, None);
    }

    #[test]
    fn nested_and_quoted_defaults_do_not_break_param_splitting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_py(
            &dir,
            "nested.py",
            "def f(a: dict[str, int], b: str = \"x,y\") -> None:\n    pass\n",
        );
        let facts = compute(&[entry(&path, "Python")]);

        assert_eq!(facts.python_def_total, 1);
        assert_eq!(facts.python_return_annotated, 1);
        assert_eq!(facts.python_fully_param_annotated, 1);
        assert_eq!(facts.python_return_annotation_coverage, Some(1.0));
        assert_eq!(facts.python_full_param_annotation_coverage, Some(1.0));
    }

    #[test]
    fn ci_and_test_source_counts_come_from_entry_paths() {
        let facts = compute(&[
            entry(".gitlab-ci.yml", "Config"),
            entry(".github/workflows/ci.yml", "Config"),
            entry("src/app.py", "Python"),
            entry("tests/test_app.py", "Python"),
            entry("README.md", "Documentation"),
        ]);

        assert_eq!(
            facts.ci_configs,
            vec![".github/workflows/ci.yml".to_string(), ".gitlab-ci.yml".to_string()]
        );
        assert_eq!(facts.test_files, 1);
        // The .py files do not exist on disk here: no def stats, but the
        // entries still count towards the file totals.
        assert_eq!(facts.source_files, 4);
        assert_eq!(facts.python_def_total, 0);
    }

    #[test]
    fn prompt_block_snapshot_with_python_and_ci() {
        let facts = RepoFacts {
            python_def_total: 4,
            python_return_annotated: 3,
            python_fully_param_annotated: 2,
            python_return_annotation_coverage: Some(0.75),
            python_full_param_annotation_coverage: Some(0.5),
            ci_configs: vec![".github/workflows/ci.yml".to_string(), ".gitlab-ci.yml".to_string()],
            test_files: 1,
            source_files: 9,
        };
        let expected = r#"repo_facts:
  python:
    def_total: 4
    return_annotated: 3
    return_annotation_coverage: 0.75
    fully_param_annotated: 2
    full_param_annotation_coverage: 0.50
  ci_configs:
    - ".github/workflows/ci.yml"
    - ".gitlab-ci.yml"
  test_files: 1
  source_files: 9
"#;
        assert_eq!(facts.to_prompt_block(), expected);
    }

    #[test]
    fn prompt_block_snapshot_without_python_or_ci() {
        let facts = RepoFacts {
            test_files: 2,
            source_files: 7,
            ..Default::default()
        };
        let expected = r#"repo_facts:
  python: null
  ci_configs: []
  test_files: 2
  source_files: 7
"#;
        assert_eq!(facts.to_prompt_block(), expected);
    }

    #[test]
    fn is_test_file_recognizes_swiftpm_structure() {
        // SwiftPM convention: `*Tests.swift` files under the uppercase `Tests/`
        // directory (e.g. `Tests/FooTests/BarTests.swift`).
        assert!(is_test_file("BarTests.swift", "Tests/FooTests/BarTests.swift"));
        assert!(is_test_file("FooTests.swift", "Tests/FooTests/FooTests.swift"));
        // Scanned paths are absolute, so the `Tests/` directory segment also
        // flags files regardless of the `Tests.swift` suffix.
        assert!(is_test_file("FooSpec.swift", "/repo/Tests/FooTests/FooSpec.swift"));
        // Regular Swift source must not be flagged as a test.
        assert!(!is_test_file("main.swift", "Sources/main.swift"));
        // Lowercase `tests/` detection is unchanged (no regression).
        assert!(is_test_file("helpers.py", "/repo/tests/helpers.py"));
    }

    #[test]
    fn is_test_file_matches_documented_conventions() {
        let cases: &[(&str, &str, bool)] = &[
            ("foo_test.rs", "src/foo_test.rs", true),
            ("utils.rs", "src/utils.rs", false),
            ("test_utils.py", "src/test_utils.py", true),
            ("utils.py", "src/utils.py", false),
            ("app.test.ts", "src/app.test.ts", true),
            ("utils.spec.js", "src/utils.spec.js", true),
            ("utils_test.go", "pkg/utils_test.go", true),
            ("FooTest.java", "src/FooTest.java", true),
            // Only the *Test.java suffix counts, not any file with "Test" in it.
            ("TestFoo.java", "src/TestFoo.java", false),
            ("helper.py", "tests/helper.py", true),
            ("helper.ts", "src/__tests__/helper.ts", true),
            ("helper.ts", "src/spec/helper.ts", true),
            // "/spec/" needs the trailing slash; "/specs/" is a different dir.
            ("helper.ts", "src/specs/helper.ts", false),
            // Sibling `tests.rs` (the `#[cfg(test)] mod tests;` split-file
            // convention used by large modules) and its `_tests.rs` variants.
            ("tests.rs", "src/output/parser/tests.rs", true),
            ("middleware_tests.rs", "src/server/auth/middleware_tests.rs", true),
            ("findings_tests.rs", "src/output/parser/findings_tests.rs", true),
            // `test.rs` (no `s`) is not the convention; `_test.rs` already matched.
            ("contest.rs", "src/contest.rs", false),
            // The `.test.` / `.spec.` / `_test.` infixes are extension-agnostic:
            // tsx/vue/etc. variants of the JS/TS conventions count too.
            ("app.test.tsx", "src/app.test.tsx", true),
            ("widget.spec.vue", "src/widget.spec.vue", true),
            ("parser_test.ts", "src/parser_test.ts", true),
            // "latest" contains "test" but not as a delimited infix.
            ("latest.rs", "src/latest.rs", false),
        ];
        for (name, path, expected) in cases {
            assert_eq!(is_test_file(name, path), *expected, "name={name} path={path}");
        }
    }

    #[test]
    fn is_ci_config_recognizes_well_known_ci_paths() {
        let cases: &[(&str, bool)] = &[
            (".gitlab-ci.yml", true),
            ("Jenkinsfile", true),
            ("azure-pipelines.yml", true),
            (".github/workflows/ci.yml", true),
            (".github/workflows/deploy.yaml", true),
            ("sub/.github/workflows/ci.yml", true),
            (".circleci/config.yml", true),
            ("sub/.circleci/config.yml", true),
            (".buildkite/pipeline.yml", true),
            ("src/main.rs", false),
            (".github/CODEOWNERS", false),
        ];
        for (path, expected) in cases {
            assert_eq!(is_ci_config(path), *expected, "path={path}");
        }
    }

    #[test]
    fn analyse_python_defs_counts_unterminated_signature_at_eof() {
        // Malformed input: the multi-line `def` never gets its trailing `:`,
        // but the heuristic still records it instead of dropping the count.
        let stats = analyse_python_defs("def f(\n    a: int\n    b: str");
        assert_eq!(stats.total, 1);
        assert_eq!(stats.return_annotated, 0);
        // No closing `)` means an empty (vacuously annotated) param list.
        assert_eq!(stats.fully_param_annotated, 1);
    }

    #[test]
    fn analyse_python_defs_caps_runaway_signatures_at_64_lines() {
        let mut content = String::from("def f(\n");
        for i in 0..100 {
            content.push_str(&format!("    p{i}: int,\n"));
        }
        let stats = analyse_python_defs(&content);
        // Exactly one def is recorded; the 64-line cap terminates the scan
        // instead of growing the accumulator forever.
        assert_eq!(stats.total, 1);
        assert_eq!(stats.return_annotated, 0);
    }

    #[test]
    fn record_signature_skips_star_and_slash_separator_markers() {
        // `*` and `/` are PEP 484/570 parameter separators, not real params;
        // both annotated params still make the signature fully annotated.
        let stats = analyse_python_defs("def f(*, a: int, /, b: int) -> None:\n    pass\n");
        assert_eq!(stats.total, 1);
        assert_eq!(stats.return_annotated, 1);
        assert_eq!(stats.fully_param_annotated, 1);
    }

    #[test]
    fn record_signature_varargs_count_as_unannotated() {
        // `*args` / `**kwargs` carry no annotation and must mark the
        // signature as NOT fully annotated.
        let stats = analyse_python_defs("def g(a: int, *args, **kwargs) -> None:\n    pass\n");
        assert_eq!(stats.total, 1);
        assert_eq!(stats.return_annotated, 1);
        assert_eq!(stats.fully_param_annotated, 0);
    }

    #[test]
    fn split_top_level_commas_respects_brackets_and_quotes() {
        let parts = split_top_level_commas("a: dict[str, int], b: str = \"x,y\", c: str = r\"p,q\"");
        // Whitespace around the separators is preserved; `record_signature`
        // trims each token before inspecting it.
        assert_eq!(
            parts,
            vec![
                "a: dict[str, int]".to_string(),
                " b: str = \"x,y\"".to_string(),
                " c: str = r\"p,q\"".to_string(),
            ]
        );
    }

    #[test]
    fn split_top_level_commas_ignores_escaped_quotes() {
        // A backslash-escaped quote inside a string must not terminate it.
        let parts = split_top_level_commas("x: str = \"a\\\"b\", y: int");
        assert_eq!(parts, vec!["x: str = \"a\\\"b\"".to_string(), " y: int".to_string()]);
    }

    #[test]
    fn cfg_test_line_spans_finds_a_single_block() {
        // Canonical sirena shape: `#[cfg(test)]` on its own line,
        // then `mod tests {`, then a body, then a closing `}`.
        let src = "\
fn real() {}\n\
#[cfg(test)]\n\
mod tests {\n\
    #[test]\n\
    fn t() {\n\
        let password = \"sk\";\n\
    }\n\
}\n";
        let spans = cfg_test_line_spans(src);
        assert_eq!(spans, vec![(2, 8)], "single cfg-test block: {spans:?}");
    }

    #[test]
    fn cfg_test_line_spans_returns_empty_without_attribute() {
        let src = "fn real() { let password = \"sk\"; }\n";
        assert!(cfg_test_line_spans(src).is_empty());
    }

    #[test]
    fn cfg_test_line_spans_handles_nested_braces() {
        // The body itself contains nested braces (a `fn` body and a
        // nested struct / map literal); the depth counter must wait
        // for the *outer* closing brace, not the first one it sees.
        let src = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        let s = Some(1).map(|x| { x + 1 });\n        let _ = s;\n    }\n}\n";
        let spans = cfg_test_line_spans(src);
        assert_eq!(
            spans,
            vec![(1, 8)],
            "nested braces must not shorten the span: {spans:?}"
        );
    }

    #[test]
    fn cfg_test_line_spans_handles_multiple_attributes_between_cfg_and_mod() {
        // A `#[cfg(test)]` followed by another attribute (commonly
        // `#[allow(...)]`) before the `mod` opener must still resolve
        // to one block. The reproducer repo carries no such companion
        // attributes, but the wider Rust ecosystem does.
        let src = "\
#[cfg(test)]\n\
#[allow(unused)]\n\
mod tests {\n\
    fn helper() {}\n\
}\n";
        let spans = cfg_test_line_spans(src);
        assert_eq!(
            spans,
            vec![(1, 5)],
            "the second attribute must not split the block: {spans:?}"
        );
    }

    #[test]
    fn cfg_test_line_spans_finds_multiple_blocks() {
        // Two distinct `#[cfg(test)] mod ...` blocks (e.g. one for unit
        // tests, one for integration-style tests in the same file) —
        // each must produce its own span.
        let src = "#[cfg(test)]\nmod unit {\n    fn a() {}\n}\n#[cfg(test)]\nmod integration {\n    fn b() {}\n}\n";
        let spans = cfg_test_line_spans(src);
        assert_eq!(spans, vec![(1, 4), (5, 8)], "two blocks: {spans:?}");
    }

    /// RENG-114 follow-up (M19 r2, reviewer-facts P2-2): a `{` inside
    /// a string literal must NOT extend the cfg-test span past the
    /// module's real closing brace. The reviewer-facts r1 reproducer:
    ///
    /// ```text
    /// fn prod_before() {}                         // L 1
    /// #[cfg(test)]                                // L 2
    /// mod tests {                                 // L 3
    ///     #[test]                                // L 4
    ///     fn brace_literal() {                   // L 5
    ///         assert_eq!(brace("{"), 1);         // L 6  — '{' inside a string
    ///     }                                      // L 7
    /// }                                          // L 8
    /// fn prod_after() {                           // L 9
    ///     let api_key = "abcdefghijklmnop";      // L10
    /// }                                          // L11
    /// ```
    ///
    /// The pre-r2 algorithm counted every `{` / `}` byte and reported
    /// span (2, 12), swallowing the production `api_key` line at L10
    /// — a false negative in the security sweep (a real leak would
    /// never reach the report). The r2 scanner only tallies braces
    /// outside string/char literals and outside comments, so the
    /// `{` inside `brace("{")` is ignored and the span ends at L8.
    #[test]
    fn cfg_test_line_spans_ignores_braces_inside_string_literal() {
        let src = "fn prod_before() {}\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                       #[test]\n\
                       fn brace_literal() {\n\
                           assert_eq!(brace(\"{\"), 1);\n\
                       }\n\
                   }\n\
                   fn prod_after() {\n\
                       let api_key = \"abcdefghijklmnop\";\n\
                   }\n";
        let spans = cfg_test_line_spans(src);
        assert_eq!(
            spans,
            vec![(2, 8)],
            "a `{{` inside a string must not over-extend the span: {spans:?}"
        );
        // And the production `api_key` line (L10) is OUTSIDE the
        // span — the security sweep must see it.
        assert!(
            !spans.iter().any(|&(s, e)| s <= 10 && 10 <= e),
            "production line 10 must be outside the span: {spans:?}"
        );
    }

    /// RENG-114 follow-up (M19 r2): a `}` inside a string must NOT
    /// prematurely close the cfg-test span. The mirror of the
    /// `{`-in-string pin; without the string-aware counter, the
    /// inner `}` ends the outer block at line 4 and leaves the real
    /// `mod tests { ... }` body un-skipped.
    #[test]
    fn cfg_test_line_spans_ignores_closing_brace_inside_string_literal() {
        let src = "#[cfg(test)]\nmod tests {\n    fn t() {\n        let s = \"}\";\n    }\n}\n";
        let spans = cfg_test_line_spans(src);
        assert_eq!(
            spans,
            vec![(1, 6)],
            "a `}}` inside a string must not prematurely close the span: {spans:?}"
        );
    }

    /// RENG-114 follow-up (M19 r2): a `{` or `}` inside a line
    /// comment (`// …`) must be ignored. Without the comment-aware
    /// counter, the depth after the `}` on line 4 is 0 and the
    /// span ends at the line-comment, leaving the real test body
    /// unscanned.
    #[test]
    fn cfg_test_line_spans_ignores_braces_inside_line_comment() {
        let src = "#[cfg(test)]\nmod tests {\n    fn t() {\n        // }\n        let x = 1;\n    }\n}\n";
        let spans = cfg_test_line_spans(src);
        assert_eq!(
            spans,
            vec![(1, 7)],
            "a `}}` inside a line comment must not close the span: {spans:?}"
        );
    }

    /// RENG-114 follow-up (M19 r2): a `/* … */` block comment that
    /// crosses a newline must stay open across the line boundary.
    /// The `in_block_comment` state threads through the scanner;
    /// without it, the `*/` on line 4 would be misread as
    /// `*` (no-op) followed by `/` (no-op) and a stray `}` two lines
    /// later would not have a paired `{` to subtract from.
    #[test]
    fn cfg_test_line_spans_handles_multi_line_block_comment() {
        let src = "#[cfg(test)]\n\
                   mod tests {\n\
                   /* block\n\
                      comment with } brace */\n\
                   fn t() {\n\
                       let x = 1;\n\
                   }\n\
                   }\n";
        let spans = cfg_test_line_spans(src);
        assert_eq!(
            spans,
            vec![(1, 8)],
            "multi-line block comment must stay open across the newline: {spans:?}"
        );
    }

    /// RENG-114 follow-up (M19 r2): the string / char literal
    /// escape rule — `\"` / `\'` must not close the literal.
    /// `"{ \"a\" }"` parses as a single string with embedded
    /// braces; under the new scanner the `{` and `}` inside the
    /// string are not counted.
    #[test]
    fn cfg_test_line_spans_handles_escaped_quotes_in_string_literal() {
        let src = "#[cfg(test)]\n\
                   mod tests {\n\
                       fn t() {\n\
                           let s = \"\\\"{\\\"\";\n\
                       }\n\
                   }\n";
        let spans = cfg_test_line_spans(src);
        assert_eq!(
            spans,
            vec![(1, 6)],
            "escaped quotes must not close the string early: {spans:?}"
        );
    }

    /// RENG-114 follow-up (M19 r2): a `\` at the end of a line in
    /// a string is Rust's line-continuation marker — the string
    /// continues on the next line. The reproducer in
    /// `src/publisher/mod.rs` (lines 1315–1322 etc.) uses the
    /// pattern. Without the per-line `in_string` state threading,
    /// the algorithm closes the string at the EOL and the next
    /// line's `{` (inside `{\"message\":…}`) is counted as code,
    /// pushing the end of the cfg-test span past the real close.
    #[test]
    fn cfg_test_line_spans_handles_line_continuation_string() {
        // The string spans L2 and L3; the `{` on L3 must be
        // inside the string, not a code brace.
        let src = "#[cfg(test)]\n\
                   mod tests {\n\
                       fn t() {\n\
                           let _ = \"part 1 \\\n\
                                    {part 2} \\\n\
                                    part 3\";\n\
                       }\n\
                   }\n";
        let spans = cfg_test_line_spans(src);
        assert_eq!(
            spans,
            vec![(1, 8)],
            "the multi-line string must keep `{{part 2}}` inside the string and not over-extend: {spans:?}"
        );
    }

    /// RENG-114 follow-up (M19 r2): a raw string `r#"…"#` must
    /// not interpret escape sequences — `\"` inside the raw
    /// string is two characters, and the `"` that follows is
    /// the closing `"`, NOT the end of a `\"` escape pair. The
    /// reproducer in `src/publisher/mod.rs` line 1171 has exactly
    /// this shape.
    #[test]
    fn cfg_test_line_spans_handles_raw_string_with_escape_chars() {
        // The raw string content is `{"message":"…\"…"}` — a
        // real JSON payload with `\"` inside. Under the scanner's
        // raw-string handling, the closing `"` is the one
        // followed by `#`, not the one that comes after `\"`.
        let src = "#[cfg(test)]\n\
                   mod tests {\n\
                       fn t() {\n\
                           let _ = r#\"{\\\"k\\\" : \\\"v\\\"}\"#;\n\
                       }\n\
                   }\n";
        let spans = cfg_test_line_spans(src);
        assert_eq!(
            spans,
            vec![(1, 6)],
            "a raw string with `\\\"` inside must not close the literal early: {spans:?}"
        );
    }

    /// RENG-114 follow-up (M19 r2 + r3, reviewer-facts counterexamples):
    /// the r1 byte-counting algorithm over-ran past the real
    /// closing brace (the r1 direction: `{` / `}` inside a string
    /// literal pushed the span past the mod close), and the r2
    /// state machine without lifetime handling had a *second* over-
    /// run direction: treating every `'` as a char-literal opener
    /// swallowed the `{` on a line that contains a Rust lifetime
    /// (`&'static str`, `'_`, `<'a>`), so the depth-0 break fired at
    /// an inner `}` and the span ended early. The r2 over-run audit
    /// (the four real-source files) caught the first; the r3 audit
    /// (the same four files) catches the second after the lifetime
    /// rule is in place — the reviewer's compiled-harness test
    /// reported r2 spans of (661, 1868) / (672, 770) / (475, 836)
    /// / (614, 1203) where the real closes are 1909 / 1549 / 1042 /
    /// 1257.
    ///
    /// The audit: the r3 end-line must point at a line whose only
    /// non-whitespace content is `}` **at column 0** — the module's
    /// real closing brace. An indented inner `}` is NOT the mod
    /// close (the r2 pin accepted `end_line.trim() == "}"`, which
    /// would pass on an inner `    }` and silently mask an
    /// early-end bug; the r3 fix tightens to `end_line == "}"`).
    /// Every file in the audit has its test module at the file tail
    /// (no production line after the mod), so the end-line is the
    /// file's last line, and the r2 algorithm returned an end-line
    /// one past the real close in every case (547 / 1910 / etc.);
    /// the r3 lifetime rule hits the brace exactly.

    #[test]
    fn cfg_test_line_spans_does_not_overrun_on_real_source_files() {
        let workspace = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo test");
        for rel in [
            "src/feedback.rs",
            "src/publisher/mod.rs",
            "src/team/adjudicator.rs",
            "src/server/dispatcher.rs",
        ] {
            let path = std::path::Path::new(&workspace).join(rel);
            let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
            let spans = cfg_test_line_spans(&src);
            assert!(!spans.is_empty(), "{rel}: must find at least one cfg-test span");
            // The last span's end-line must be the module's real
            // closing brace at column 0 — NOT an indented inner `}`.
            // (The r2 assertion `end_line.trim() == "}"` accepted
            // indented inner closes and was a false green; the r3
            // pin is `end_line == "}"`, exactly the column-0 brace.)
            let (_, e) = spans.last().unwrap();
            let end_line = src.lines().nth(*e - 1).unwrap_or("");
            assert!(
                end_line == "}",
                "{rel}: last span ends at line {e} ({end_line:?}); r3 must close at the module's column-0 `}}` brace: spans={spans:?}"
            );
        }
    }

    /// RENG-114 follow-up (M19 r3, reviewer-facts counterexample):
    /// Rust lifetimes (`&'static str`, `'_`, `<'a>`) must NOT be
    /// parsed as char literals. The reviewer reproduced the
    /// RENG-114 false positive with this minimal fixture:
    /// ```text
    /// #[cfg(test)]
    /// mod tests {
    ///     fn a(&self) -> &'static str { "x" }
    ///     #[test]
    ///     fn t() { let password = "sk"; }
    /// }
    /// fn prod() {}
    /// ```
    /// The r2 algorithm (every `'` opens a char-literal scan)
    /// reported span (1, 5) here: the `'` in `&'static` started a
    /// scan that consumed the line's `{`, depth went from 1 to 0
    /// at the first `}` it saw, and the span closed early. The
    /// security sweep then reported `Hardcoded password` on the
    /// `let password = "sk";` line at L8, which is inside the test
    /// module — the RENG-114 false positive the mission closes.
    /// The r3 lifetime rule (only `'\…'` or `'<X>'` opens a char
    /// literal) keeps the span at (1, 8) so the password line is
    /// suppressed.
    #[test]
    fn cfg_test_line_spans_treats_lifetimes_as_lifetimes_not_char_literals() {
        let src = "\
#[cfg(test)]\n\
mod tests {\n\
    fn a(&self) -> &'static str {\n\
        \"x\"\n\
    }\n\
    #[test]\n\
    fn t() {\n\
        let password = \"sk\";\n\
    }\n\
}\n\
fn prod() {}\n";
        let spans = cfg_test_line_spans(src);
        assert_eq!(
            spans,
            vec![(1, 10)],
            "the r3 lifetime rule must keep the cfg-test span open through the `&'static` line: {spans:?}"
        );
    }
}
