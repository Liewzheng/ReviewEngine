//! Helpers shared by the crate's own unit tests.
//!
//! Declared as `#[cfg(test)] mod test_util;` in `lib.rs`, so it is compiled only
//! when the library is built for tests and never reaches the shipped crate.
//! Integration tests under `tests/` link the library compiled *without*
//! `cfg(test)` and therefore cannot see these helpers.

/// Parse the count that precedes `unit` in an assessment TL;DR, e.g.
/// `parse_tldr_count("Risk Level: Low. 2 critical, 3 high found by 1 reviewers.", "high") == 3`.
/// Returns 0 when the phrase is absent.
///
/// Unit tests pair this with the severity histogram of the report's *published*
/// findings and assert the two agree — the RENG-73 invariant. The prose counts
/// used to come from the raw per-expert reports while the total came from the
/// consolidated list, so the summary named severities the shipped list did not
/// contain.
pub(crate) fn parse_tldr_count(tl_dr: &str, unit: &str) -> usize {
    for part in tl_dr.split([',', '.']) {
        let tokens: Vec<&str> = part.split_whitespace().collect();
        if let Some(pos) = tokens.iter().position(|t| *t == unit) {
            if pos > 0 {
                if let Ok(n) = tokens[pos - 1].parse::<usize>() {
                    return n;
                }
            }
        }
    }
    0
}
