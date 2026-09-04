//! Placeholder rewriting for the dual-backend `Any` pool.
//!
//! Store SQL is written with `?` placeholders (the only spelling SQLite
//! accepts positionally). sqlx 0.8.6's `Any` driver passes the statement
//! text through verbatim — it does NOT translate `?` to `$1..$n` (verified
//! by grepping `sqlx-core-0.8.6/src/any/` and by 0.10.0 E2E on real PG,
//! where every bound DML failed with `42601 syntax error`). The store layer
//! therefore rewrites placeholders itself before executing on PostgreSQL;
//! SQLite gets the text unchanged ([`crate::store::SqlxStore::sql`]).
//!
//! The rewriter is a small lexer, not a string replace: `?` inside
//! single-quoted string literals (with `''` escaping), double-quoted
//! identifiers, `--` line comments and `/* */` block comments is literal
//! text, not a bind slot. Backslash is NOT an escape in standard SQL string
//! literals (PG runs with `standard_conforming_strings=on` by default), so
//! `'\'` is a complete one-char literal; only `''` escapes a quote.

/// Rewrite every top-level `?` in `sql` to `$1..$n` (positional, in order of
/// appearance). `?` inside string literals, quoted identifiers, or comments
/// is left untouched. The input is assumed syntactically valid; unterminated
/// literals/comments are copied verbatim (the database will reject them).
pub(crate) fn rewrite(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len() + 8);
    let mut chars = sql.chars().peekable();
    let mut n: u32 = 0;
    while let Some(c) = chars.next() {
        match c {
            '?' => {
                n += 1;
                out.push('$');
                out.push_str(&n.to_string());
            }
            '\'' | '"' => {
                let quote = c;
                out.push(c);
                let it = chars.by_ref();
                while let Some(inner) = it.next() {
                    out.push(inner);
                    if inner == quote {
                        // A doubled quote is an escaped literal quote: copy
                        // it and stay inside the literal.
                        if it.peek() == Some(&quote) {
                            out.push(quote);
                            it.next();
                        } else {
                            break;
                        }
                    }
                }
            }
            '-' if chars.peek() == Some(&'-') => {
                out.push('-');
                out.push('-');
                chars.next();
                for inner in chars.by_ref() {
                    out.push(inner);
                    if inner == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                out.push('/');
                out.push('*');
                chars.next();
                // PostgreSQL nests block comments; depth tracking keeps `?`
                // inside nested sections literal too (SQLite sees none of
                // this — it takes the original text).
                let mut depth = 1u32;
                let mut prev = '\0';
                for inner in chars.by_ref() {
                    out.push(inner);
                    if prev == '/' && inner == '*' {
                        depth += 1;
                    } else if prev == '*' && inner == '/' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    prev = inner;
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::rewrite;

    #[test]
    fn simple_placeholders_are_numbered_in_order() {
        assert_eq!(
            rewrite("SELECT * FROM t WHERE a = ? AND b = ?"),
            "SELECT * FROM t WHERE a = $1 AND b = $2"
        );
    }

    #[test]
    fn no_placeholders_passes_through() {
        let sql = "DELETE FROM git_platforms";
        assert_eq!(rewrite(sql), sql);
    }

    #[test]
    fn consecutive_and_double_digit_placeholders() {
        let sql = format!("VALUES ({})", ["?"; 12].join(", "));
        let expected = format!(
            "VALUES ({})",
            (1..=12).map(|i| format!("${i}")).collect::<Vec<_>>().join(", ")
        );
        assert_eq!(rewrite(&sql), expected);
        // Trailing placeholder at end of statement.
        assert_eq!(rewrite("DELETE FROM t WHERE id = ?"), "DELETE FROM t WHERE id = $1");
    }

    #[test]
    fn single_quoted_literal_preserves_question_mark() {
        // The interrupted-sweep UPDATE shape: literal text containing `?`
        // alongside a real bind slot.
        assert_eq!(
            rewrite("UPDATE reviews SET error = 'interrupted: why?' WHERE task_id = ?"),
            "UPDATE reviews SET error = 'interrupted: why?' WHERE task_id = $1"
        );
    }

    #[test]
    fn escaped_single_quote_keeps_literal_open() {
        // `''` is an escaped quote, so the `?` after it is still inside the
        // literal; only the final one is a bind slot.
        assert_eq!(
            rewrite("INSERT INTO t (a, b) VALUES ('it''s a ?', ?)"),
            "INSERT INTO t (a, b) VALUES ('it''s a ?', $1)"
        );
    }

    #[test]
    fn backslash_does_not_escape_in_standard_literals() {
        // standard_conforming_strings=on: '\' is one complete literal; the
        // `?` after it is a bind slot.
        assert_eq!(
            rewrite("LOWER(source_meta) LIKE LOWER(?) ESCAPE '\\' AND state = ?"),
            "LOWER(source_meta) LIKE LOWER($1) ESCAPE '\\' AND state = $2"
        );
    }

    #[test]
    fn double_quoted_identifier_preserves_question_mark() {
        assert_eq!(
            rewrite("SELECT \"weird?column\" FROM t WHERE id = ?"),
            "SELECT \"weird?column\" FROM t WHERE id = $1"
        );
        // Escaped `""` keeps the identifier open.
        assert_eq!(
            rewrite("SELECT \"a\"\"?b\" FROM t WHERE id = ?"),
            "SELECT \"a\"\"?b\" FROM t WHERE id = $1"
        );
    }

    #[test]
    fn line_comment_preserves_question_mark() {
        assert_eq!(
            rewrite("SELECT ? -- trailing ? in comment\nWHERE x = ?"),
            "SELECT $1 -- trailing ? in comment\nWHERE x = $2"
        );
        // A lone `-` is an operator, not a comment start.
        assert_eq!(rewrite("SELECT a - ? FROM t"), "SELECT a - $1 FROM t");
    }

    #[test]
    fn block_comment_preserves_question_mark_including_nested() {
        assert_eq!(
            rewrite("SELECT * FROM t /* filter: ? */ WHERE x = ?"),
            "SELECT * FROM t /* filter: ? */ WHERE x = $1"
        );
        // PG nests block comments: the `?` inside the nested section is
        // literal, and the outer comment does not end early.
        assert_eq!(
            rewrite("/* outer /* nested ? */ still comment ? */ SELECT ?"),
            "/* outer /* nested ? */ still comment ? */ SELECT $1"
        );
        // A lone `/` is a division operator.
        assert_eq!(rewrite("SELECT a / ? FROM t"), "SELECT a / $1 FROM t");
    }

    #[test]
    fn unterminated_literal_is_copied_verbatim() {
        // Degenerate input the database will reject anyway: the rewriter
        // must not invent placeholders inside it.
        assert_eq!(
            rewrite("INSERT INTO t VALUES ('oops ?"),
            "INSERT INTO t VALUES ('oops ?"
        );
    }

    #[test]
    fn mixed_real_statement_from_list_reviews() {
        // The assembled shape of the history-list page query: literal
        // backslash, LIKE needle, and trailing LIMIT/OFFSET slots.
        let sql = "SELECT task_id FROM reviews WHERE LOWER(source_meta) LIKE LOWER(?) ESCAPE '\\' \
                   AND created_at >= ? ORDER BY created_at DESC LIMIT ? OFFSET ?";
        let expected = "SELECT task_id FROM reviews WHERE LOWER(source_meta) LIKE LOWER($1) ESCAPE '\\' \
                        AND created_at >= $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4";
        assert_eq!(rewrite(sql), expected);
    }
}
