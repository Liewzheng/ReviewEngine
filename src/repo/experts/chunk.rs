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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::FileEntry;

    /// Create `name` under `dir` with the given content and return its path
    /// as the scanner would (absolute).
    fn write_file(dir: &std::path::Path, name: &str, content: &str) -> String {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn entry(path: &str, is_binary: bool, is_generated: bool) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            language: "Rust".to_string(),
            loc: std::fs::read_to_string(path).map(|c| c.lines().count()).unwrap_or(0),
            is_binary,
            is_generated,
        }
    }

    /// Build a temp repo tree and return (root, entries).
    fn fixture() -> (tempfile::TempDir, Vec<FileEntry>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let a = write_file(root, "src/server/http.rs", "fn serve() {}\n");
        let b = write_file(root, "src/server/db.rs", "fn query() {}\nfn pool() {}\n");
        let c = write_file(
            root,
            "src/server/router.rs",
            "fn route() {}\nfn mount() {}\nfn wrap() {}\n",
        );
        let d = write_file(root, "src/client/api.rs", "fn call() {}\n");
        let e = write_file(root, "README.md", "# readme\n");
        let entries = vec![
            entry(&a, false, false),
            entry(&b, false, false),
            entry(&c, false, false),
            entry(&d, false, false),
            entry(&e, false, false),
        ];
        (dir, entries)
    }

    #[test]
    fn groups_files_by_two_level_module_dir() {
        let (dir, entries) = fixture();
        let chunks = chunk_by_module(&entries, dir.path());

        // src/server has 3 files → its own chunk; src/client has 1 → merged
        // into the largest chunk (src/server); README is "other".
        let server = chunks
            .iter()
            .find(|c| c.module == "src/server")
            .expect("src/server chunk");
        assert_eq!(
            server.files.len(),
            4,
            "undersized src/client merges into the largest chunk"
        );
        assert!(server.files.iter().any(|p| p.ends_with("src/client/api.rs")));
        let other = chunks.iter().find(|c| c.module == "other").expect("other chunk");
        assert_eq!(other.files.len(), 1);
        assert!(other.files[0].ends_with("README.md"));
    }

    #[test]
    fn module_key_uses_first_two_path_components() {
        let dir = tempfile::tempdir().unwrap();
        let deep = write_file(dir.path(), "a/b/c/d.rs", "fn deep() {}\n");
        let chunks = chunk_by_module(&[entry(&deep, false, false)], dir.path());
        assert_eq!(chunks[0].module, "a/b");
    }

    #[test]
    fn files_outside_root_land_in_other() {
        let dir = tempfile::tempdir().unwrap();
        let outside = write_file(dir.path(), "outside.rs", "fn out() {}\n");
        // Root is a subdir, so `outside.rs` sits outside it.
        let sub = dir.path().join("repo");
        std::fs::create_dir_all(&sub).unwrap();
        let chunks = chunk_by_module(&[entry(&outside, false, false)], &sub);
        assert_eq!(chunks[0].module, "other");
    }

    #[test]
    fn binary_and_generated_entries_are_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let ok = write_file(dir.path(), "src/app/main.rs", "fn main() {}\n");
        let bin = write_file(dir.path(), "src/app/binary.bin", "binary");
        let gen = write_file(dir.path(), "src/app/generated.rs", "// generated\n");
        let chunks = chunk_by_module(
            &[
                entry(&ok, false, false),
                entry(&bin, true, false),
                entry(&gen, false, true),
            ],
            dir.path(),
        );
        assert_eq!(chunks.len(), 1, "only the non-binary non-generated file remains");
        assert_eq!(chunks[0].files.len(), 1);
        assert!(chunks[0].files[0].ends_with("main.rs"));
    }

    #[test]
    fn total_loc_sums_concatenated_file_lines() {
        let (dir, entries) = fixture();
        let chunks = chunk_by_module(&entries, dir.path());
        let server = chunks.iter().find(|c| c.module == "src/server").unwrap();
        // http.rs(1) + db.rs(2) + router.rs(3) + api.rs(1) = 7.
        assert_eq!(server.total_loc, 7);
    }

    #[test]
    fn code_concatenation_is_header_then_content_joined_by_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_file(dir.path(), "src/app/a.rs", "fn a() {}\n");
        let b = write_file(dir.path(), "src/app/b.rs", "fn b() {}\nfn b2() {}\n");
        let chunks = chunk_by_module(&[entry(&a, false, false), entry(&b, false, false)], dir.path());
        assert_eq!(chunks.len(), 1);
        let code = &chunks[0].code;
        assert!(code.contains(&format!("// --- {a} ---\nfn a() {{}}")));
        assert!(code.contains(&format!("// --- {b} ---\nfn b() {{}}\nfn b2() {{}}")));
        assert!(code.contains("\n\n"), "chunks separated by a blank line");
    }

    #[test]
    fn single_large_module_stays_alone_without_merging() {
        let dir = tempfile::tempdir().unwrap();
        let mut entries = Vec::new();
        for i in 0..3 {
            let p = write_file(dir.path(), &format!("src/mod/x{i}.rs"), "fn x() {}\n");
            entries.push(entry(&p, false, false));
        }
        let chunks = chunk_by_module(&entries, dir.path());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].module, "src/mod");
        assert_eq!(chunks[0].files.len(), 3);
    }

    #[test]
    fn empty_entries_produce_no_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let chunks = chunk_by_module(&[], dir.path());
        assert!(chunks.is_empty());
    }

    #[test]
    fn unreadable_file_yields_empty_content_but_stays_in_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(dir.path(), "src/app/missing.rs", "fn gone() {}\n");
        std::fs::remove_file(&p).unwrap(); // path exists in entry but not on disk
        let chunks = chunk_by_module(&[entry(&p, false, false)], dir.path());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].total_loc, 0, "unreadable content contributes 0 LOC");
    }

    #[test]
    fn code_chunk_paths_are_sorted_by_module_btreemap() {
        let dir = tempfile::tempdir().unwrap();
        let mut entries = Vec::new();
        // 3 files per module so neither chunk is undersized and merged away.
        for i in 0..3 {
            let zebra = write_file(dir.path(), &format!("src/zebra/z{i}.rs"), "fn z() {}\n");
            let alpha = write_file(dir.path(), &format!("src/alpha/a{i}.rs"), "fn a() {}\n");
            entries.push(entry(&zebra, false, false));
            entries.push(entry(&alpha, false, false));
        }
        let mut chunks = chunk_by_module(&entries, dir.path());
        chunks.sort_by(|x, y| x.module.cmp(&y.module));
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].module, "src/alpha");
        assert_eq!(chunks[1].module, "src/zebra");
    }

    #[test]
    fn code_chunk_is_debug_cloneable() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_file(dir.path(), "src/app/a.rs", "fn a() {}\n");
        let chunks = chunk_by_module(&[entry(&a, false, false)], dir.path());
        let _dbg = format!("{:?}", chunks[0]);
        let _clone = chunks[0].clone();
    }
}
