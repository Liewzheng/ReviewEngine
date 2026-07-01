use std::collections::BTreeMap;

use crate::repo::FileEntry;

/// Minimum files per chunk. Smaller chunks are merged into neighbors.
const MIN_FILES_PER_CHUNK: usize = 3;

/// A single chunk of the repository assigned to an LLM expert.
#[derive(Debug, Clone)]
pub struct CodeChunk {
    pub module: String,     // directory path, e.g. "src/server"
    pub files: Vec<String>, // file paths in this chunk
    pub total_loc: usize,
    pub code: String, // concatenated file contents
}

/// Group file entries into chunks by top-level directory, then merge
/// undersized chunks into nearby larger ones.
///
/// Strategy:
/// - Each `src/<module>/` directory becomes its own chunk.
/// - Chunks with fewer than `MIN_FILES_PER_CHUNK` files are merged
///   into the nearest sibling or into `other` as fallback.
/// - Files directly under `src/` are grouped as `src/other`.
/// - Non-`src/` files (docs, config, scripts) are grouped as `other`.
/// - Binary and generated files are excluded.
pub fn chunk_by_module(entries: &[FileEntry], root: &std::path::Path) -> Vec<CodeChunk> {
    let mut groups: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();

    for entry in entries {
        if entry.is_binary || entry.is_generated {
            continue;
        }
        let path = std::path::Path::new(&entry.path);

        // Determine module key
        let module = if let Some(relative) = path.strip_prefix(root).ok() {
            let comps: Vec<_> = relative.components().collect();
            if comps.len() >= 2 {
                let dir = comps[0].as_os_str().to_string_lossy();
                let sub = comps[1].as_os_str().to_string_lossy();
                format!("{dir}/{sub}")
            } else {
                String::from("other")
            }
        } else {
            String::from("other")
        };

        let content = match std::fs::read_to_string(&entry.path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Failed to read {} for chunking: {:?}", entry.path, e);
                String::new()
            }
        };
        groups.entry(module).or_default().push((entry.path.clone(), content));
    }

    // ── Merge undersized chunks ──
    let mut small: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut large: Vec<(String, Vec<(String, String)>)> = Vec::new();

    for (module, files) in groups {
        if module == "other" {
            // "other" is the default catchment — always keep it
            large.push((module, files));
        } else if files.len() < MIN_FILES_PER_CHUNK {
            small.push((module, files));
        } else {
            large.push((module, files));
        }
    }

    // Merge small chunks into large ones
    for (small_mod, small_files) in small {
        if let Some((_, ref mut target)) = large.iter_mut().max_by_key(|(_, f)| f.len()) {
            target.extend(small_files);
        } else {
            large.push((small_mod, small_files));
        }
    }

    // ── Build CodeChunks ──
    large
        .into_iter()
        .map(|(module, files)| {
            let total_loc: usize = files.iter().map(|(_, c)| c.lines().count()).sum();
            let code = files
                .iter()
                .map(|(path, content)| format!("// --- {path} ---\n{content}"))
                .collect::<Vec<_>>()
                .join("\n\n");
            let file_paths: Vec<String> = files.into_iter().map(|(p, _)| p).collect();
            CodeChunk {
                module,
                files: file_paths,
                total_loc,
                code,
            }
        })
        .collect()
}
