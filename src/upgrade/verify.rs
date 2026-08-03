//! Checksum verification and safe archive extraction for downloaded assets.
//!
//! Two failure modes are defended here:
//!
//! * **Corruption / MITM** — every downloaded asset is verified against its
//!   `<asset>.sha256` sidecar before it is trusted or extracted.
//! * **Zip-slip** — an attacker-controlled archive can name an entry `../../x`
//!   or `/etc/x` to write outside the extraction root. We validate **every**
//!   entry (including symlink/hardlink targets) before writing **any** file,
//!   so a hostile archive is rejected atomically with no partial extraction.
//!
//! Decompression is also capped (1 GiB per tar.gz stream / per zip entry) so a
//! decompression bomb cannot exhaust memory or disk during the update path.

use std::io::Read;
use std::path::Path;

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};

use super::error::{Result, UpgradeError};
use super::platform::AssetFormat;

/// Upper bound for a single decompressed tar.gz stream.
const MAX_DECOMPRESSED_BYTES: u64 = 1024 * 1024 * 1024;
/// Upper bound for the declared uncompressed size of a single zip entry.
const MAX_ENTRY_UNCOMPRESSED: u64 = 1024 * 1024 * 1024;

/// Lowercase hex SHA-256 of `data`.
pub fn data_sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Lowercase hex SHA-256 of a file's contents (streamed, no whole-file load).
pub fn file_sha256_hex(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Parse one checksum line in GNU `sha256sum` format: `<hex>  <filename>`.
///
/// Whitespace-separated; tolerates the `*filename` binary-mode marker and
/// leading/trailing whitespace. Returns `(lowercase_hex, filename)`.
pub fn parse_sha256_line(line: &str) -> Result<(String, String)> {
    let line = line.trim();
    if line.is_empty() {
        return Err(UpgradeError::invalid_data("empty checksum line"));
    }
    let mut parts = line.split_whitespace();
    let hex = parts
        .next()
        .ok_or_else(|| UpgradeError::invalid_data("checksum line missing hex value"))?;
    let filename = parts.next().unwrap_or("");
    let hex = normalize_hex(hex)?;
    let filename = filename.strip_prefix('*').unwrap_or(filename);
    Ok((hex, filename.to_string()))
}

/// Verify that `path`'s SHA-256 matches `expected_hex` (case-insensitive).
pub fn verify_file_sha256(path: &Path, expected_hex: &str) -> Result<()> {
    let expected = normalize_hex(expected_hex)?;
    let actual = file_sha256_hex(path)?;
    if actual == expected {
        Ok(())
    } else {
        Err(UpgradeError::checksum_mismatch(expected, actual))
    }
}

/// Verify a file against a `.sha256` file's text.
///
/// Skips blank lines and `#` comments; uses the first real checksum line.
/// Returns the matching hex on success.
pub fn verify_file_with_checksum_text(path: &Path, checksum_text: &str) -> Result<String> {
    for line in checksum_text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (hex, _filename) = parse_sha256_line(line)?;
        verify_file_sha256(path, &hex)?;
        return Ok(hex);
    }
    Err(UpgradeError::invalid_data("checksum file contains no checksum lines"))
}

/// `true` when an archive entry path cannot escape its extraction root.
///
/// Rejects: empty paths, NUL bytes, absolute paths (POSIX `/...` or Windows
/// `\...` / `C:\...` / UNC), and any `..` path component. Both `/` and `\`
/// are treated as separators so a name crafted for one OS is caught on any OS.
pub fn is_safe_entry_path(entry_path: &str) -> bool {
    if entry_path.is_empty() || entry_path.contains('\0') {
        return false;
    }
    let bytes = entry_path.as_bytes();
    if bytes[0] == b'/' || bytes[0] == b'\\' {
        return false;
    }
    // Windows drive letter or UNC device prefix, e.g. `C:evil` / `C:\evil`.
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return false;
    }
    for component in entry_path.split(|c| c == '/' || c == '\\') {
        if component == ".." {
            return false;
        }
    }
    true
}

/// Extract a `.tar.gz` archive into `dest`, atomically rejecting any entry
/// that could escape the extraction root.
pub fn extract_tar_gz(archive_path: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(archive_path)?;
    // Cap decompression so a gzip bomb cannot exhaust memory.
    let mut gz = GzDecoder::new(file).take(MAX_DECOMPRESSED_BYTES + 1);
    let mut decompressed = Vec::new();
    gz.read_to_end(&mut decompressed)
        .map_err(|e| UpgradeError::archive(format!("corrupt gzip stream: {e}")))?;
    if decompressed.len() as u64 > MAX_DECOMPRESSED_BYTES {
        return Err(UpgradeError::archive("tar.gz decompresses beyond the 1 GiB safety cap"));
    }

    // Pass 1 — validate every entry before writing anything.
    {
        let mut archive = tar::Archive::new(std::io::Cursor::new(&decompressed));
        let entries = archive
            .entries()
            .map_err(|e| UpgradeError::archive(format!("corrupt tar: {e}")))?;
        let mut count = 0usize;
        for entry in entries {
            let entry = entry.map_err(|e| UpgradeError::archive(format!("corrupt tar entry: {e}")))?;
            let name = entry
                .path()
                .map_err(|e| UpgradeError::archive(format!("invalid entry path: {e}")))?
                .to_string_lossy()
                .into_owned();
            if !is_safe_entry_path(&name) {
                return Err(UpgradeError::unsafe_entry(name));
            }
            // Symlinks/hardlinks are rejected outright: a link whose target
            // points outside `dest` is a write-through escape vector even when
            // the link's own name is safe.
            let entry_type = entry.header().entry_type();
            if entry_type.is_symlink() || entry_type.is_hard_link() {
                return Err(UpgradeError::unsafe_entry(format!(
                    "{name} (symlink/hardlink entries are not allowed in release archives)"
                )));
            }
            count += 1;
        }
        if count == 0 {
            return Err(UpgradeError::invalid_data("archive contains no entries"));
        }
    }

    // Pass 2 — extract the fully-validated archive.
    std::fs::create_dir_all(dest)?;
    let mut archive = tar::Archive::new(std::io::Cursor::new(&decompressed));
    archive
        .unpack(dest)
        .map_err(|e| UpgradeError::archive(format!("extract failed: {e}")))?;
    Ok(())
}

/// Extract a `.zip` archive into `dest`, atomically rejecting any entry that
/// could escape the extraction root.
pub fn extract_zip(archive_path: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| UpgradeError::archive(format!("corrupt zip: {e}")))?;

    // Pass 1 — validate every entry (name, declared size, link flag).
    let mut validated: Vec<(String, bool)> = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let entry = archive
            .by_index_raw(i)
            .map_err(|e| UpgradeError::archive(format!("corrupt zip entry: {e}")))?;
        let name = entry.name().to_string();
        if !is_safe_entry_path(&name) {
            return Err(UpgradeError::unsafe_entry(name));
        }
        if entry.size() > MAX_ENTRY_UNCOMPRESSED {
            return Err(UpgradeError::archive(format!(
                "entry {name:?} declares {size} bytes, exceeding the {MAX_ENTRY_UNCOMPRESSED} safety cap",
                size = entry.size()
            )));
        }
        let is_symlink = entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000);
        if is_symlink {
            return Err(UpgradeError::unsafe_entry(format!(
                "{name} (symlink entries are not allowed in release archives)"
            )));
        }
        validated.push((name, is_symlink));
    }
    if validated.is_empty() {
        return Err(UpgradeError::invalid_data("archive contains no entries"));
    }

    // Pass 2 — extract the fully-validated archive.
    std::fs::create_dir_all(dest)?;
    for i in 0..archive.len() {
        let (name, _is_symlink) = &validated[i];
        let mut entry = archive
            .by_index(i)
            .map_err(|e| UpgradeError::archive(format!("corrupt zip entry: {e}")))?;
        let out_path = dest.join(name);
        if name.ends_with('/') {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&out_path)?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

/// Extract an asset archive according to its format.
pub fn extract_asset(archive_path: &Path, format: AssetFormat, dest: &Path) -> Result<()> {
    match format {
        AssetFormat::TarGz => extract_tar_gz(archive_path, dest),
        AssetFormat::Zip => extract_zip(archive_path, dest),
    }
}

fn normalize_hex(s: &str) -> Result<String> {
    let s = s.trim().to_ascii_lowercase();
    if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(s)
    } else {
        Err(UpgradeError::invalid_data(format!("invalid sha256 hex: {s:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    const HELLO_SHA256: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    // ─── checksums ─────────────────────────────────────────────

    #[test]
    fn known_sha256_vector() {
        assert_eq!(data_sha256_hex(b"hello"), HELLO_SHA256);
    }

    #[test]
    fn file_verify_ok_and_mismatch() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("data.bin");
        std::fs::write(&file, b"hello").unwrap();

        assert_eq!(file_sha256_hex(&file).unwrap(), HELLO_SHA256);
        assert!(verify_file_sha256(&file, HELLO_SHA256).is_ok());
        // Uppercase hex must also pass (normalization).
        assert!(verify_file_sha256(&file, &HELLO_SHA256.to_uppercase()).is_ok());

        // Flip one nibble → mismatch with both hex values in the error.
        let wrong = format!("0{}", &HELLO_SHA256[1..]);
        let err = verify_file_sha256(&file, &wrong).unwrap_err();
        assert!(matches!(err, UpgradeError::ChecksumMismatch { .. }));
        assert!(err.to_string().contains(&wrong));

        // Malformed hex is invalid data, not a mismatch.
        assert!(verify_file_sha256(&file, "nothex").is_err());
    }

    #[test]
    fn parse_sha256_line_variants() {
        let (hex, name) = parse_sha256_line(&format!("{HELLO_SHA256}  data.bin")).unwrap();
        assert_eq!(hex, HELLO_SHA256);
        assert_eq!(name, "data.bin");

        // Binary-mode `*` marker.
        let (_, name) = parse_sha256_line(&format!("{HELLO_SHA256} *data.bin")).unwrap();
        assert_eq!(name, "data.bin");

        // Extra whitespace around the line.
        let (hex, _) = parse_sha256_line(&format!("  {HELLO_SHA256}   data.bin  ")).unwrap();
        assert_eq!(hex, HELLO_SHA256);

        // Missing filename is tolerated (hex only).
        let (hex, name) = parse_sha256_line(HELLO_SHA256).unwrap();
        assert_eq!(hex, HELLO_SHA256);
        assert_eq!(name, "");

        // Wrong length / non-hex rejected.
        assert!(parse_sha256_line("deadbeef  x.bin").is_err());
        assert!(parse_sha256_line("").is_err());
        assert!(parse_sha256_line("   ").is_err());
    }

    #[test]
    fn verify_with_checksum_text_skips_comments() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("asset.bin");
        std::fs::write(&file, b"hello").unwrap();

        let text = format!("# generated by CI\n\n{HELLO_SHA256}  asset.bin\n");
        let hex = verify_file_with_checksum_text(&file, &text).unwrap();
        assert_eq!(hex, HELLO_SHA256);

        // No checksum lines at all → error.
        assert!(verify_file_with_checksum_text(&file, "# nothing here\n").is_err());
    }

    // ─── entry path safety ─────────────────────────────────────

    #[test]
    fn safe_path_matrix() {
        assert!(is_safe_entry_path("bin/review-engine"));
        assert!(is_safe_entry_path("review-engine"));
        assert!(is_safe_entry_path("a/b/c"));
        assert!(is_safe_entry_path("dir/"));

        assert!(!is_safe_entry_path(""), "empty path");
        assert!(!is_safe_entry_path(".."), "bare parent");
        assert!(!is_safe_entry_path("../evil"), "parent escape");
        assert!(!is_safe_entry_path("a/../../evil"), "nested escape");
        assert!(!is_safe_entry_path("a/.."), "trailing parent");
        assert!(!is_safe_entry_path("/abs/evil"), "absolute POSIX");
        assert!(!is_safe_entry_path("\\abs\\evil"), "absolute backslash");
        assert!(!is_safe_entry_path("C:/evil"), "windows drive slash");
        assert!(!is_safe_entry_path("C:\\evil"), "windows drive backslash");
        assert!(!is_safe_entry_path("C:evil"), "windows drive no sep");
        assert!(!is_safe_entry_path("a/../b"), "internal parent");
        assert!(!is_safe_entry_path("a\\..\\b"), "internal parent backslash");
    }

    // ─── tar.gz extraction ─────────────────────────────────────

    fn write_tar_gz<F>(path: &Path, build: F) -> std::io::Result<()>
    where
        F: FnOnce(&mut tar::Builder<flate2::write::GzEncoder<std::fs::File>>) -> std::io::Result<()>,
    {
        let file = std::fs::File::create(path)?;
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        build(&mut builder)?;
        let encoder = builder.into_inner()?;
        encoder.finish()?;
        Ok(())
    }

    fn file_entry(
        builder: &mut tar::Builder<flate2::write::GzEncoder<std::fs::File>>,
        name: &str,
        data: &[u8],
    ) -> std::io::Result<()> {
        let mut header = tar::Header::new_gnu();
        header.set_path(name)?;
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, data)?;
        Ok(())
    }

    // Build a tar.gz from raw tar bytes so tests can plant entries that the
    // `tar` crate's own writer refuses to emit (`..`, absolute paths, links).
    // This exercises our *reader-side* validation against hostile archives.
    fn raw_tar_gz(path: &Path, entries: &[(&str, u8, &[u8], Option<&str>)]) {
        fn put_octal(buf: &mut [u8], range: std::ops::Range<usize>, value: u64) {
            let digits = range.len() - 1;
            let s = format!("{value:0digits$o}");
            assert!(s.len() == digits, "octal value {value} too wide for field");
            buf[range.start..range.end - 1].copy_from_slice(s.as_bytes());
            buf[range.end - 1] = 0; // NUL terminator
        }

        let mut out = Vec::new();
        for (name, typeflag, data, linkname) in entries {
            let mut h = [0u8; 512];
            let nb = name.as_bytes();
            assert!(nb.len() <= 100, "raw tar test name too long: {name:?}");
            h[..nb.len()].copy_from_slice(nb);
            put_octal(&mut h, 100..108, 0o644);
            put_octal(&mut h, 108..116, 0);
            put_octal(&mut h, 116..124, 0);
            put_octal(&mut h, 124..136, data.len() as u64);
            put_octal(&mut h, 136..148, 0);
            h[156] = *typeflag;
            if let Some(target) = linkname {
                let lb = target.as_bytes();
                assert!(lb.len() <= 100, "raw tar link target too long");
                h[157..157 + lb.len()].copy_from_slice(lb);
            }
            h[257..263].copy_from_slice(b"ustar\0");
            h[263..265].copy_from_slice(b"00");
            // Checksum: sum of all bytes with the chksum field treated as spaces.
            let sum: u32 = h[..148].iter().map(|b| *b as u32).sum::<u32>()
                + 8 * (b' ' as u32)
                + h[156..].iter().map(|b| *b as u32).sum::<u32>();
            let chksum = format!("{sum:06o}\0 ");
            assert!(chksum.len() == 8);
            h[148..156].copy_from_slice(chksum.as_bytes());

            out.extend_from_slice(&h);
            out.extend_from_slice(data);
            let pad = (512 - data.len() % 512) % 512;
            out.extend(std::iter::repeat(0u8).take(pad));
        }
        // Two zero blocks terminate the archive.
        out.extend_from_slice(&[0u8; 1024]);

        let file = std::fs::File::create(path).unwrap();
        let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &out).unwrap();
        encoder.finish().unwrap();
    }

    #[test]
    fn tar_extract_ok() {
        let dir = tempdir().unwrap();
        let archive = dir.path().join("good.tar.gz");
        write_tar_gz(&archive, |b| {
            file_entry(b, "bin/review-engine", b"#!/bin/sh\necho hi\n")?;
            file_entry(b, "LICENSE", b"Apache-2.0\n")?;
            Ok(())
        })
        .unwrap();

        let out = dir.path().join("out");
        extract_tar_gz(&archive, &out).unwrap();
        let binary = std::fs::read(out.join("bin/review-engine")).unwrap();
        assert_eq!(binary, b"#!/bin/sh\necho hi\n");
        assert_eq!(std::fs::read(out.join("LICENSE")).unwrap(), b"Apache-2.0\n");
    }

    #[test]
    fn tar_extract_rejects_zip_slip() {
        for evil in ["../evil.txt", "a/../../evil.txt", "a/.."] {
            let dir = tempdir().unwrap();
            let archive = dir.path().join("bad.tar.gz");
            raw_tar_gz(&archive, &[("bin/ok", b'0', b"x", None), (evil, b'0', b"evil", None)]);

            let out = dir.path().join("out");
            let err = extract_tar_gz(&archive, &out).unwrap_err();
            assert!(
                matches!(err, UpgradeError::UnsafeEntry(_)),
                "expected UnsafeEntry for {evil:?}, got {err:?}"
            );
            assert!(!out.exists(), "no partial extraction for {evil:?}");
        }
    }

    #[test]
    fn tar_extract_rejects_absolute_path() {
        for evil in ["/etc/passwd", "\\evil.exe"] {
            let dir = tempdir().unwrap();
            let archive = dir.path().join("bad.tar.gz");
            raw_tar_gz(&archive, &[(evil, b'0', b"evil", None)]);
            let out = dir.path().join("out");
            let err = extract_tar_gz(&archive, &out).unwrap_err();
            assert!(matches!(err, UpgradeError::UnsafeEntry(_)), "got {err:?}");
        }
    }

    #[test]
    fn tar_extract_rejects_symlink_and_hardlink() {
        let dir = tempdir().unwrap();
        let archive = dir.path().join("links.tar.gz");
        raw_tar_gz(
            &archive,
            &[
                ("bin/app", b'0', b"payload", None),
                ("escape", b'2', b"", Some("/etc/passwd")), // symlink
                ("hard", b'1', b"", Some("/etc/shadow")),   // hard link
            ],
        );
        let out = dir.path().join("out");
        let err = extract_tar_gz(&archive, &out).unwrap_err();
        assert!(matches!(err, UpgradeError::UnsafeEntry(_)), "got {err:?}");
        assert!(!out.exists(), "links must fail before any extraction");
    }

    #[test]
    fn tar_extract_rejects_empty_archive() {
        let dir = tempdir().unwrap();
        let archive = dir.path().join("empty.tar.gz");
        write_tar_gz(&archive, |_| Ok(())).unwrap();
        let out = dir.path().join("out");
        let err = extract_tar_gz(&archive, &out).unwrap_err();
        assert!(matches!(err, UpgradeError::InvalidData(_)), "got {err:?}");
    }

    #[test]
    fn tar_extract_rejects_corrupt_gzip() {
        let dir = tempdir().unwrap();
        let archive = dir.path().join("corrupt.tar.gz");
        std::fs::write(&archive, b"this is not gzip").unwrap();
        let out = dir.path().join("out");
        assert!(extract_tar_gz(&archive, &out).is_err());
    }

    // ─── zip extraction ────────────────────────────────────────

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn zip_extract_ok() {
        let dir = tempdir().unwrap();
        let archive = dir.path().join("good.zip");
        write_zip(
            &archive,
            &[("bin/review-engine.exe", b"MZ-fake"), ("README.txt", b"readme")],
        );

        let out = dir.path().join("out");
        extract_zip(&archive, &out).unwrap();
        assert_eq!(std::fs::read(out.join("bin/review-engine.exe")).unwrap(), b"MZ-fake");
        assert_eq!(std::fs::read(out.join("README.txt")).unwrap(), b"readme");
    }

    #[test]
    fn zip_extract_rejects_zip_slip() {
        for evil in [
            "../evil.txt",
            "a/../../evil.txt",
            "/abs.txt",
            "C:\\evil.txt",
            "C:/evil.txt",
        ] {
            let dir = tempdir().unwrap();
            let archive = dir.path().join("bad.zip");
            write_zip(&archive, &[("ok.txt", b"ok"), (evil, b"evil")]);

            let out = dir.path().join("out");
            let err = extract_zip(&archive, &out).unwrap_err();
            assert!(
                matches!(err, UpgradeError::UnsafeEntry(_)),
                "expected UnsafeEntry for {evil:?}, got {err:?}"
            );
            assert!(!out.exists(), "no partial extraction for {evil:?}");
        }
    }

    #[test]
    fn zip_extract_rejects_symlink_entry() {
        let dir = tempdir().unwrap();
        let archive = dir.path().join("link.zip");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            writer
                .add_symlink("escape", "/etc/passwd", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.finish().unwrap();
        }
        let out = dir.path().join("out");
        let err = extract_zip(&archive, &out).unwrap_err();
        assert!(matches!(err, UpgradeError::UnsafeEntry(_)), "got {err:?}");
        assert!(!out.exists());
    }

    #[test]
    fn zip_extract_rejects_empty() {
        let dir = tempdir().unwrap();
        let archive = dir.path().join("empty.zip");
        write_zip(&archive, &[]);
        let out = dir.path().join("out");
        let err = extract_zip(&archive, &out).unwrap_err();
        assert!(matches!(err, UpgradeError::InvalidData(_)), "got {err:?}");
    }

    #[test]
    fn extract_asset_dispatches() {
        let dir = tempdir().unwrap();
        let archive = dir.path().join("x.zip");
        write_zip(&archive, &[("a.txt", b"a")]);
        let out = dir.path().join("zip-out");
        extract_asset(&archive, AssetFormat::Zip, &out).unwrap();
        assert!(out.join("a.txt").exists());

        let tar = dir.path().join("x.tar.gz");
        write_tar_gz(&tar, |b| file_entry(b, "a.txt", b"a")).unwrap();
        let out2 = dir.path().join("tar-out");
        extract_asset(&tar, AssetFormat::TarGz, &out2).unwrap();
        assert!(out2.join("a.txt").exists());
    }
}
