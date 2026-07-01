//! Markdown sanitization helpers for LLM-generated review content.
//!
//! LLM outputs often contain fenced code blocks (` ``` `) that are not
//! properly closed. When those fields are inserted into a larger Markdown
//! report, an unclosed fence swallows subsequent headings and paragraphs
//! into the code block. The helpers in this module detect and close those
//! fences before rendering.

/// Close any unclosed markdown code fence at the end of `text`.
///
/// Only backtick fences are handled. The algorithm tracks the currently
/// open fence marker (e.g. ` ``` ` or ` ```rust `) line-by-line; if the
/// text ends while a fence is still open, the matching closing fence is
/// appended.
///
/// # Examples
///
/// ```rust
/// use review_engine::output::markdown::close_unclosed_code_fences;
///
/// let text = "```rust\nlet x = 1;\n";
/// assert_eq!(close_unclosed_code_fences(text), "```rust\nlet x = 1;\n```");
/// ```
pub fn close_unclosed_code_fences(text: &str) -> String {
    let mut open_fence: Option<String> = None;

    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(marker) = parse_fence_marker(trimmed) {
            match open_fence {
                Some(ref open) if open == marker => open_fence = None,
                None => open_fence = Some(marker.to_string()),
                _ => {}
            }
        }
    }

    match open_fence {
        Some(marker) => {
            let mut out = text.to_string();
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&marker);
            out
        }
        None => text.to_string(),
    }
}

fn parse_fence_marker(line: &str) -> Option<&str> {
    if !line.starts_with("```") {
        return None;
    }
    let end = line.find(|c: char| c != '`').unwrap_or(line.len());
    Some(&line[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_balanced_fences_unchanged() {
        let text = "```rust\nlet x = 1;\n```";
        assert_eq!(close_unclosed_code_fences(text), text);
    }

    #[test]
    fn test_unclosed_fence_gets_closed() {
        let text = "```rust\nlet x = 1;\n";
        assert_eq!(close_unclosed_code_fences(text), "```rust\nlet x = 1;\n```");
    }

    #[test]
    fn test_plain_text_unchanged() {
        let text = "No code here.";
        assert_eq!(close_unclosed_code_fences(text), text);
    }

    #[test]
    fn test_nested_different_length_fences() {
        // Outer fence is 3 backticks; inner fenced content uses 4.
        let text = "```\n````\nnested\n````\n";
        assert_eq!(close_unclosed_code_fences(text), "```\n````\nnested\n````\n```");
    }

    #[test]
    fn test_mismatched_language_tags() {
        let text = "```rust\nlet x = 1;\n```\n\n```python\nprint(x)\n";
        assert_eq!(
            close_unclosed_code_fences(text),
            "```rust\nlet x = 1;\n```\n\n```python\nprint(x)\n```"
        );
    }

    #[test]
    fn test_no_trailing_newline() {
        let text = "```\nfoo";
        assert_eq!(close_unclosed_code_fences(text), "```\nfoo\n```");
    }
}
