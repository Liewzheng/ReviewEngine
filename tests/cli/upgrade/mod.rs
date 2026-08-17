use super::*;

// ─────────────────────────────────────────────────────────────────────
// `upgrade` subcommand — self-update.
//
// The upgrade library (`review_engine::upgrade`) hardcodes the GitHub API
// base URL and only exposes its test seam to its own unit tests, so these
// CLI tests drive the binary through the documented env overrides in
// `src/cli/handlers.rs`:
//   REVIEW_UPGRADE_TEST_RELEASE     fake release metadata (bypasses GitHub API)
//   REVIEW_UPGRADE_CURRENT_VERSION  fake current version
//   REVIEW_UPGRADE_INSTALL_METHOD   force the install method
//   REVIEW_UPGRADE_EXE              target exe for self-replace / rollback
// Asset and checksum downloads are served by a local wiremock server.
// ─────────────────────────────────────────────────────────────────────

fn shasum(data: &[u8]) -> String {
    review_engine::upgrade::verify::data_sha256_hex(data)
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

/// Build a single-entry `.tar.gz` (stored deflate block) containing `content`
/// as `name` with the given octal mode — valid enough for the `tar` crate.
fn single_file_tar_gz(name: &str, content: &[u8], mode: u32) -> Vec<u8> {
    let mut h = [0u8; 512];
    let nb = name.as_bytes();
    assert!(nb.len() <= 100, "tar name too long: {name}");
    h[..nb.len()].copy_from_slice(nb);
    h[100..108].copy_from_slice(format!("{mode:07o}\0").as_bytes());
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    h[124..136].copy_from_slice(format!("{:011o}\0", content.len() as u64).as_bytes());
    h[136..148].copy_from_slice(b"00000000000\0");
    h[156] = b'0'; // regular file
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum: u32 = h[..148].iter().map(|b| *b as u32).sum::<u32>()
        + 8 * (b' ' as u32)
        + h[156..].iter().map(|b| *b as u32).sum::<u32>();
    let chksum = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(chksum.as_bytes());

    let mut tar = Vec::new();
    tar.extend_from_slice(&h);
    tar.extend_from_slice(content);
    let pad = (512 - content.len() % 512) % 512;
    tar.extend(std::iter::repeat(0u8).take(pad));
    tar.extend_from_slice(&[0u8; 1024]);

    let mut out = Vec::new();
    out.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff]);
    out.push(0x01); // final, stored deflate block
    let len = u16::try_from(tar.len()).expect("archive too large for one stored block");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes());
    out.extend_from_slice(&tar);
    out.extend_from_slice(&crc32(&tar).to_le_bytes());
    out.extend_from_slice(&(tar.len() as u32).to_le_bytes());
    out
}

fn fake_binary(version: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho \"Review Engine v{version}\"\n").into_bytes()
}

fn test_release_json(tag: &str, asset_url: &str, asset_size: u64, checksum_url: &str, checksum_size: u64) -> String {
    format!(
        r#"{{"tag":"{tag}","asset_name":"review-engine-test.tar.gz","asset_url":"{asset_url}","asset_size":{asset_size},"checksum_url":"{checksum_url}","checksum_size":{checksum_size}}}"#
    )
}

fn run_with_env(args: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new(bin_path());
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("failed to execute review-engine")
}

/// Mount a wiremock release: the asset archive plus a two-line `.sha256`
/// sidecar (line 1 = archive hash for `download_verified_asset`, line 2 =
/// binary hash for the post-extract double-check). Returns (asset_url,
/// checksum_url, asset_size, checksum_size).
async fn mount_release(server: &wiremock::MockServer, archive: &[u8], binary_hex: &str) -> (String, String, u64, u64) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    let sidecar = format!(
        "{}  review-engine-test.tar.gz\n{}  review-engine\n",
        shasum(archive),
        binary_hex
    );
    Mock::given(method("GET"))
        .and(path("/asset.tar.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(archive.to_vec()))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/asset.tar.gz.sha256"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sidecar.clone()))
        .mount(server)
        .await;
    (
        format!("{}/asset.tar.gz", server.uri()),
        format!("{}/asset.tar.gz.sha256", server.uri()),
        archive.len() as u64,
        sidecar.len() as u64,
    )
}

mod core;
mod misc;
mod serve;
