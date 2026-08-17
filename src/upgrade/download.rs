//! Download release assets to a uniquely-named temp file.
//!
//! Ownership contract (who cleans up when things fail):
//!
//! * `download_asset` owns the temp file it creates. If the download fails for
//!   any reason — HTTP error, size mismatch, I/O error — the temp file is
//!   removed before the error is returned. The caller never sees a partial
//!   file.
//! * On success the caller owns the temp file and must rename it (or delete
//!   it) once done. `download_verified_asset` does the rename for you after
//!   the SHA-256 check passes.
//!
//! Temp names embed the process id plus a random nonce, so concurrent
//! downloads into the same directory can never collide.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use tokio::io::AsyncWriteExt;

use super::error::{Result, UpgradeError};
use super::github_release::ReleaseAsset;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const READ_TIMEOUT: Duration = Duration::from_secs(300);

/// A download HTTP client: long read timeout (large binary assets) and a
/// review-engine user agent.
fn download_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(READ_TIMEOUT)
        .user_agent(concat!("review-engine/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(UpgradeError::from)
}

/// Download `url` into `dest_dir` under a unique hidden temp name.
///
/// Returns `(temp_path, bytes_written)`. `asset_name` only shapes the temp
/// file name for debuggability; uniqueness comes from pid + nonce. When
/// `expected_size` is known (from the GitHub API), any deviation — stream
/// longer than expected or shorter — fails the download.
pub async fn download_asset(
    url: &str,
    dest_dir: &Path,
    asset_name: &str,
    expected_size: Option<u64>,
) -> Result<(PathBuf, u64)> {
    tokio::fs::create_dir_all(dest_dir).await?;
    let client = download_client()?;
    let temp_path = temp_path_for(dest_dir, asset_name);
    let result = download_to_temp(&client, &temp_path, url, expected_size).await;
    if result.is_err() {
        // Best-effort cleanup: never hand the caller a partial file.
        let _ = tokio::fs::remove_file(&temp_path).await;
    }
    result
}

/// Download `asset` and its `<prefix>-<triple>.sha256` sidecar, verify the checksum, and
/// rename the verified file into place at `dest_dir/<asset.name>`.
///
/// On any failure both temp files are removed and `dest_dir` is left as it
/// was. Returns the final path of the verified asset.
pub async fn download_verified_asset(
    asset: &ReleaseAsset,
    checksum_asset: &ReleaseAsset,
    dest_dir: &Path,
) -> Result<PathBuf> {
    tokio::fs::create_dir_all(dest_dir).await?;
    let client = download_client()?;
    let asset_temp = temp_path_for(dest_dir, &asset.name);
    let checksum_temp = temp_path_for(dest_dir, &checksum_asset.name);

    let result = async {
        let (asset_path, _) = download_to_temp(&client, &asset_temp, &asset.download_url, Some(asset.size)).await?;
        let (checksum_path, _) = download_to_temp(
            &client,
            &checksum_temp,
            &checksum_asset.download_url,
            Some(checksum_asset.size),
        )
        .await?;
        let checksum_text = tokio::fs::read_to_string(&checksum_path).await?;
        super::verify::verify_file_with_checksum_text(&asset_path, &checksum_text)?;
        Ok::<_, UpgradeError>(asset_path)
    }
    .await;

    match result {
        Ok(asset_path) => {
            let final_path = dest_dir.join(&asset.name);
            // Temp and final share a directory, hence a filesystem: rename is
            // atomic and cannot leave a torn file behind.
            tokio::fs::rename(&asset_path, &final_path).await?;
            let _ = tokio::fs::remove_file(&checksum_temp).await;
            Ok(final_path)
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&asset_temp).await;
            let _ = tokio::fs::remove_file(&checksum_temp).await;
            Err(e)
        }
    }
}

async fn download_to_temp(
    client: &reqwest::Client,
    temp_path: &Path,
    url: &str,
    expected_size: Option<u64>,
) -> Result<(PathBuf, u64)> {
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        return Err(UpgradeError::Api {
            status,
            body: format!("downloading {url}"),
        });
    }

    let mut file = tokio::fs::File::create(temp_path).await?;
    let mut stream = resp.bytes_stream();
    let mut written: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(UpgradeError::from)?;
        file.write_all(&chunk).await?;
        written += chunk.len() as u64;
        if let Some(expected) = expected_size {
            if written > expected {
                return Err(UpgradeError::invalid_data(format!(
                    "download exceeded expected size {expected} bytes (got {written})"
                )));
            }
        }
    }
    file.flush().await?;
    file.sync_all().await?;
    drop(file);

    if let Some(expected) = expected_size {
        if written != expected {
            return Err(UpgradeError::invalid_data(format!(
                "download size mismatch: expected {expected} bytes, got {written}"
            )));
        }
    }
    Ok((temp_path.to_path_buf(), written))
}

/// A unique temp path like `dest/.review-engine-<asset>-<pid>-<nonce>.tmp`.
fn temp_path_for(dest_dir: &Path, asset_name: &str) -> PathBuf {
    let slug: String = asset_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let nonce: u64 = rand::random();
    dest_dir.join(format!(".{slug}.{}.{:x}.tmp", std::process::id(), nonce))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn downloads_to_unique_temp_and_counts_bytes() {
        let server = MockServer::start().await;
        let body = b"binary-data-123".to_vec();
        Mock::given(method("GET"))
            .and(path("/asset.tar.gz"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let (temp, written) = download_asset(
            &format!("{}/asset.tar.gz", server.uri()),
            dir.path(),
            "review-engine-x86_64-unknown-linux-gnu.tar.gz",
            Some(body.len() as u64),
        )
        .await
        .unwrap();
        assert_eq!(written, body.len() as u64);
        assert_eq!(std::fs::read(&temp).unwrap(), body);

        let name = temp.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with('.'), "temp file should be hidden, got {name}");
        assert!(name.ends_with(".tmp"), "temp file should carry .tmp, got {name}");
        std::fs::remove_file(&temp).unwrap();
    }

    #[tokio::test]
    async fn two_concurrent_downloads_do_not_collide() {
        let server = MockServer::start().await;
        let body = b"payload".to_vec();
        Mock::given(method("GET"))
            .and(path("/asset"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let url = format!("{}/asset", server.uri());
        let (a, _) = download_asset(&url, dir.path(), "asset", None).await.unwrap();
        let (b, _) = download_asset(&url, dir.path(), "asset", None).await.unwrap();
        assert_ne!(a, b, "temp paths must be unique");
        assert_eq!(std::fs::read(&a).unwrap(), body);
        assert_eq!(std::fs::read(&b).unwrap(), body);
        std::fs::remove_file(&a).unwrap();
        std::fs::remove_file(&b).unwrap();
    }

    #[tokio::test]
    async fn size_mismatch_fails_and_leaves_no_temp_file() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/asset"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"abc".to_vec()))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let url = format!("{}/asset", server.uri());
        let err = download_asset(&url, dir.path(), "asset", Some(5)).await.unwrap_err();
        assert!(matches!(err, UpgradeError::InvalidData(_)), "got {err:?}");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "failed download must clean up its temp file"
        );
    }

    #[tokio::test]
    async fn http_error_surfaces_status_and_cleans_up() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/asset"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let url = format!("{}/asset", server.uri());
        let err = download_asset(&url, dir.path(), "asset", None).await.unwrap_err();
        assert!(matches!(err, UpgradeError::Api { status: 404, .. }), "got {err:?}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn download_verified_asset_happy_path() {
        let server = MockServer::start().await;
        let data = b"the real binary".to_vec();
        let hex = super::super::verify::data_sha256_hex(&data);
        let checksum_text = format!("{hex}  review-engine-x86_64-unknown-linux-gnu.tar.gz");

        Mock::given(method("GET"))
            .and(path("/asset.tar.gz"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(data.clone()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/asset.tar.gz.sha256"))
            .respond_with(ResponseTemplate::new(200).set_body_string(checksum_text.clone()))
            .mount(&server)
            .await;

        let asset = ReleaseAsset {
            name: "review-engine-x86_64-unknown-linux-gnu.tar.gz".to_string(),
            download_url: format!("{}/asset.tar.gz", server.uri()),
            size: data.len() as u64,
        };
        let checksum = ReleaseAsset {
            name: "review-engine-x86_64-unknown-linux-gnu.sha256".to_string(),
            download_url: format!("{}/asset.tar.gz.sha256", server.uri()),
            size: checksum_text.len() as u64,
        };

        let dir = tempfile::tempdir().unwrap();
        let final_path = download_verified_asset(&asset, &checksum, dir.path()).await.unwrap();
        assert_eq!(final_path.file_name().unwrap().to_string_lossy(), asset.name);
        assert_eq!(std::fs::read(&final_path).unwrap(), data);
        // Only the final file remains — both temp files were consumed/removed.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn download_verified_asset_rejects_bad_checksum_and_cleans_up() {
        let server = MockServer::start().await;
        let data = b"tampered-or-mitm".to_vec();
        // Deliberately wrong checksum.
        let bad_hex = "0".repeat(63) + "f";

        Mock::given(method("GET"))
            .and(path("/asset.tar.gz"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(data.clone()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/asset.tar.gz.sha256"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!("{bad_hex}  asset.tar.gz")))
            .mount(&server)
            .await;

        let asset = ReleaseAsset {
            name: "review-engine-x86_64-unknown-linux-gnu.tar.gz".to_string(),
            download_url: format!("{}/asset.tar.gz", server.uri()),
            size: data.len() as u64,
        };
        let checksum_text = format!("{bad_hex}  asset.tar.gz");
        let checksum = ReleaseAsset {
            name: "review-engine-x86_64-unknown-linux-gnu.sha256".to_string(),
            download_url: format!("{}/asset.tar.gz.sha256", server.uri()),
            size: checksum_text.len() as u64,
        };

        let dir = tempfile::tempdir().unwrap();
        let err = download_verified_asset(&asset, &checksum, dir.path())
            .await
            .unwrap_err();
        assert!(matches!(err, UpgradeError::ChecksumMismatch { .. }), "got {err:?}");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "failed verification must leave nothing behind"
        );
    }

    #[test]
    fn temp_path_is_prefixed_with_dot_and_ends_in_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path_for(dir.path(), "review-engine-x86_64.tar.gz");
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with('.'), "hidden temp file: {name}");
        assert!(name.ends_with(".tmp"), "temp extension: {name}");
        assert!(name.contains("review-engine-x86_64.tar.gz"));
        assert_eq!(path.parent(), Some(dir.path()));
    }

    #[test]
    fn temp_path_sanitizes_unsafe_asset_characters() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path_for(dir.path(), "weird name/with&specials");
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(!name.contains('/'), "slashes must be replaced, got {name}");
        assert!(!name.contains('&'), "ampersand must be replaced, got {name}");
        assert!(name.contains("weird_name_with_specials"), "slugified: {name}");
    }

    #[test]
    fn temp_path_is_unique_per_call() {
        let dir = tempfile::tempdir().unwrap();
        let a = temp_path_for(dir.path(), "asset.bin");
        let b = temp_path_for(dir.path(), "asset.bin");
        assert_ne!(a, b, "nonce must make each temp path unique");
        let c = temp_path_for(dir.path(), "other.bin");
        assert_ne!(a, c);
    }

    #[test]
    fn temp_path_contains_process_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path_for(dir.path(), "a.bin");
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(
            name.contains(&std::process::id().to_string()),
            "pid must appear in {name}"
        );
    }
}
