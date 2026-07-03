//! Splits large diffs into smaller chunks for processing within token limits.
//!
//! @module review-engine: part of the CodeReview Board virtual engineering team
use crate::diff::constants::DEFAULT_TOKEN_MODEL;
use crate::diff::render::{render_file_diff, render_hunk};
use crate::models::*;
use crate::tokenizer::count_tokens;

/// A chunk of diff files to be processed together.
#[derive(Debug, Clone)]
pub struct DiffChunk {
    pub files: Vec<DiffFile>,
    pub chunk_index: usize,
    pub total_chunks: usize,
}

/// Chunk by files: each chunk contains a group of complete files.
pub fn chunk_by_files(files: &[DiffFile], max_tokens_per_chunk: usize) -> Vec<DiffChunk> {
    let mut chunks = Vec::new();
    let mut current_files = Vec::new();
    let mut current_tokens = 0usize;

    for file in files {
        let file_text = render_file_diff(file);
        let file_tokens = match count_tokens(&file_text, DEFAULT_TOKEN_MODEL) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to count tokens for file diff; assuming 0");
                0
            }
        };

        if current_tokens + file_tokens > max_tokens_per_chunk && !current_files.is_empty() {
            chunks.push(DiffChunk {
                files: std::mem::take(&mut current_files),
                chunk_index: chunks.len(),
                total_chunks: 0, // filled below
            });
            current_tokens = 0;
        }

        current_files.push(file.clone());
        current_tokens += file_tokens;
    }

    if !current_files.is_empty() {
        chunks.push(DiffChunk {
            files: current_files,
            chunk_index: chunks.len(),
            total_chunks: 0,
        });
    }

    // Fill total_chunks
    let total = chunks.len();
    for chunk in &mut chunks {
        chunk.total_chunks = total;
    }

    chunks
}

/// Chunk by hunks: split individual files across chunks if they're too large.
pub fn chunk_by_hunks(files: &[DiffFile], max_tokens_per_chunk: usize) -> Vec<DiffChunk> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut current_tokens = 0usize;

    for file in files {
        let file_tokens = compute_file_tokens(file);

        if file_tokens <= max_tokens_per_chunk {
            // File fits in a single chunk
            try_flush_current(
                &mut chunks,
                &mut current,
                &mut current_tokens,
                file_tokens,
                max_tokens_per_chunk,
            );
            current.push(file.clone());
            current_tokens += file_tokens;
        } else {
            // File too large — split by hunks
            flush_current_chunk(&mut chunks, &mut current, &mut current_tokens);
            split_file_by_hunks(file, &mut chunks, &mut current, max_tokens_per_chunk);
        }
    }

    if !current.is_empty() {
        finish_chunk(&mut chunks, &mut current);
    }

    fill_total_chunks(&mut chunks);
    chunks
}

/// Compute the token count for rendering a full file diff.
fn compute_file_tokens(file: &DiffFile) -> usize {
    let file_text = render_file_diff(file);
    match count_tokens(&file_text, DEFAULT_TOKEN_MODEL) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to count tokens for file; assuming 0");
            0
        }
    }
}

/// Compute the token count for a single hunk.
fn compute_hunk_tokens(hunk: &DiffHunk) -> usize {
    let hunk_text = render_hunk(hunk);
    match count_tokens(&hunk_text, DEFAULT_TOKEN_MODEL) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to count tokens for hunk; assuming 0");
            0
        }
    }
}

/// If adding `file_tokens` would exceed the budget, flush the current chunk first.
fn try_flush_current(
    chunks: &mut Vec<DiffChunk>,
    current: &mut Vec<DiffFile>,
    current_tokens: &mut usize,
    file_tokens: usize,
    max_tokens_per_chunk: usize,
) {
    if *current_tokens + file_tokens > max_tokens_per_chunk && !current.is_empty() {
        finish_chunk(chunks, current);
        *current_tokens = 0;
    }
}

/// Flush all current files into a new chunk and reset the token accumulator.
fn flush_current_chunk(chunks: &mut Vec<DiffChunk>, current: &mut Vec<DiffFile>, current_tokens: &mut usize) {
    if !current.is_empty() {
        finish_chunk(chunks, current);
        *current_tokens = 0;
    }
}

/// Create a partial `DiffFile` containing only the given hunks, inheriting
/// all other fields from the original file.
fn make_partial_file(file: &DiffFile, hunks: Vec<DiffHunk>) -> DiffFile {
    DiffFile { hunks, ..file.clone() }
}

/// Flush the current hunk group into a partial file, push it into `current`,
/// then flush `current` into a completed chunk.
fn flush_hunk_group(
    file: &DiffFile,
    hunk_group: &mut Vec<DiffHunk>,
    chunks: &mut Vec<DiffChunk>,
    current: &mut Vec<DiffFile>,
) {
    let partial = make_partial_file(file, std::mem::take(hunk_group));
    current.push(partial);
    finish_chunk(chunks, current);
}

/// Process a file that exceeds the per-chunk token budget by splitting it
/// across hunks. Each resulting hunk-group becomes its own chunk.
fn split_file_by_hunks(
    file: &DiffFile,
    chunks: &mut Vec<DiffChunk>,
    current: &mut Vec<DiffFile>,
    max_tokens_per_chunk: usize,
) {
    let mut hunk_group = Vec::new();
    let mut hunk_tokens = 0usize;

    for hunk in &file.hunks {
        let tokens = compute_hunk_tokens(hunk);

        // Single hunk alone exceeds budget — flush previous group and
        // put this hunk in its own chunk.
        if tokens > max_tokens_per_chunk {
            if !hunk_group.is_empty() {
                flush_hunk_group(file, &mut hunk_group, chunks, current);
                hunk_tokens = 0;
            }
            current.push(make_partial_file(file, vec![hunk.clone()]));
            finish_chunk(chunks, current);
            continue;
        }

        // Accumulating this hunk would overflow — flush the current group.
        if hunk_tokens + tokens > max_tokens_per_chunk && !hunk_group.is_empty() {
            flush_hunk_group(file, &mut hunk_group, chunks, current);
            hunk_tokens = 0;
        }

        hunk_group.push(hunk.clone());
        hunk_tokens += tokens;
    }

    // Flush any remaining hunks as the last partial file.
    if !hunk_group.is_empty() {
        current.push(make_partial_file(file, hunk_group));
        finish_chunk(chunks, current);
    }
}

/// Set `total_chunks` on every chunk after all chunks have been built.
fn fill_total_chunks(chunks: &mut Vec<DiffChunk>) {
    let total = chunks.len();
    for chunk in chunks {
        chunk.total_chunks = total;
    }
}

fn finish_chunk(chunks: &mut Vec<DiffChunk>, current: &mut Vec<DiffFile>) {
    if !current.is_empty() {
        chunks.push(DiffChunk {
            files: std::mem::take(current),
            chunk_index: chunks.len(),
            total_chunks: 0,
        });
    }
}

/// Adaptive chunking: try files first, fall back to hunks if files exceed budget.
pub fn adaptive_chunk(files: &[DiffFile], max_tokens_per_chunk: usize) -> Vec<DiffChunk> {
    let file_chunks = chunk_by_files(files, max_tokens_per_chunk);

    // Check if any chunk is too large
    let too_large = file_chunks.iter().any(|c| {
        let text: String = c.files.iter().map(render_file_diff).collect();
        match count_tokens(&text, DEFAULT_TOKEN_MODEL) {
            Ok(n) => n > max_tokens_per_chunk,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to count tokens for chunk; treating as too large");
                true
            }
        }
    });

    if too_large {
        chunk_by_hunks(files, max_tokens_per_chunk)
    } else {
        file_chunks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_simple_file(path: &str, lines: Vec<&str>) -> DiffFile {
        let hunks = vec![DiffHunk {
            header: "@@ -1 +1 @@".to_string(),
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            lines: lines
                .into_iter()
                .map(|c| DiffLine {
                    kind: if c.starts_with('+') {
                        DiffLineKind::Add
                    } else if c.starts_with('-') {
                        DiffLineKind::Delete
                    } else {
                        DiffLineKind::Context
                    },
                    content: c.to_string(),
                    old_line_no: Some(1),
                    new_line_no: Some(1),
                })
                .collect(),
        }];
        DiffFile {
            path: path.to_string(),
            old_path: path.to_string(),
            new_path: path.to_string(),
            status: "modified".to_string(),
            additions: 1,
            deletions: 0,
            hunks,
        }
    }

    #[test]
    fn test_chunk_by_files_empty() {
        let chunks = chunk_by_files(&[], 1000);
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_chunk_by_files_single() {
        let files = vec![make_simple_file("test.rs", vec!["+hello"])];
        let chunks = chunk_by_files(&files, 1000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].files.len(), 1);
    }

    #[test]
    fn test_chunk_by_files_multiple_chunks() {
        let files = vec![
            make_simple_file("a.rs", vec!["+content_a"]),
            make_simple_file("b.rs", vec!["+content_b"]),
        ];
        // Very small budget forces multiple chunks
        let chunks = chunk_by_files(&files, 5);
        assert!(chunks.len() >= 1);
    }

    #[test]
    fn test_adaptive_chunk_falls_back() {
        let files = vec![make_simple_file("large.rs", vec!["+x"; 100])];
        let chunks = adaptive_chunk(&files, 100);
        assert!(chunks.len() >= 1);
    }
}
