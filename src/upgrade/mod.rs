//! Self-update support shared by the CLI upgrade command and the web status
//! endpoint.
//!
//! Flow:
//! 1. Query GitHub Releases for the latest stable `vX.Y.Z` (`github_release`).
//! 2. Map the current platform to its release asset (`platform`).
//! 3. Detect how the binary was installed and derive upgrade hints
//!    (`install_method`).
//! 4. Download + SHA-256 verify + safely extract the new binary
//!    (`download`, `verify`).
//!
//! `check_for_updates` is the entry point; the lower-level pieces are public
//! so the CLI/Web layers can drive the flow step by step (e.g. only download
//! when the user confirms).

pub mod download;
pub mod error;
pub mod github_release;
pub mod install_method;
pub mod platform;
pub mod verify;
pub mod version;

pub use error::{Result, UpgradeError};
pub use github_release::{find_asset, find_checksum_asset, GitHubReleaseClient, Release, ReleaseAsset};
pub use install_method::InstallMethod;
pub use platform::{current_asset_spec, AssetFormat, AssetSpec};
pub use version::Version;

/// Result of a `check_for_updates` call.
/// Result of a `check_for_updates` call.
///
/// Contains the comparison between the current and latest versions,
/// the matching release asset for the current platform, and the
/// detected installation method for upgrade command generation.
#[derive(Debug, Clone)]
pub struct UpdateCheck {
    /// Currently installed version.
    pub current_version: Version,
    /// Full release metadata from GitHub.
    pub latest_release: Release,
    /// Latest available stable version.
    pub latest_version: Version,
    /// `true` when `latest_version > current_version`.
    pub has_update: bool,
    /// `None` when the current platform has no published release asset.
    pub platform: Option<AssetSpec>,
    /// The asset for the current platform, when present in the release.
    pub asset: Option<ReleaseAsset>,
    /// The `<prefix>-<triple>.sha256` sidecar, when present.
    pub checksum_asset: Option<ReleaseAsset>,
    /// Detected installation method (brew, cargo, docker, etc.).
    pub install_method: InstallMethod,
}

impl UpdateCheck {
    /// Short upgrade command for the detected install method.
    pub fn upgrade_command(&self) -> &'static str {
        self.install_method.upgrade_command()
    }

    /// Longer human-readable upgrade explanation.
    pub fn upgrade_description(&self) -> &'static str {
        self.install_method.description()
    }
}

/// Check for a newer stable release, using `CARGO_PKG_VERSION` as the current
/// version.
///
/// This never fails merely because the platform is unsupported — an
/// unsupported platform yields `platform: None` and the caller can still show
/// the install-method hint. It fails when the network/API is unreachable or no
/// stable release exists.
pub async fn check_for_updates() -> Result<UpdateCheck> {
    check_for_updates_with_version(env!("CARGO_PKG_VERSION")).await
}

/// Check for a newer stable release against an explicit current version
/// (testable and usable by the web layer which may report a different build).
pub async fn check_for_updates_with_version(current: &str) -> Result<UpdateCheck> {
    let client = GitHubReleaseClient::new(current)?;
    let latest_release = client
        .latest_stable_release()
        .await?
        .ok_or_else(|| UpgradeError::not_found("no stable vX.Y.Z release found on GitHub"))?;
    let latest_version = Version::parse_release_tag(&latest_release.tag_name).ok_or_else(|| {
        UpgradeError::invalid_data(format!(
            "latest release tag {:?} is not a stable vX.Y.Z tag",
            latest_release.tag_name
        ))
    })?;
    let current_version = Version::parse(current)?;

    let platform = current_asset_spec().ok();
    let asset = platform.as_ref().and_then(|spec| find_asset(&latest_release, spec));
    let checksum_asset = asset.and_then(|a| github_release::find_checksum_asset(&latest_release, &a.name));

    Ok(UpdateCheck {
        current_version,
        latest_version,
        has_update: latest_version > current_version,
        platform,
        asset: asset.cloned(),
        checksum_asset: checksum_asset.cloned(),
        install_method: InstallMethod::detect(),
        latest_release,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_check_compares_versions() {
        // Build an UpdateCheck directly and assert the has_update semantics.
        let release = Release {
            tag_name: "v0.9.0".to_string(),
            html_url: "https://example.com".to_string(),
            published_at: "2024-06-01T00:00:00Z".to_string(),
            assets: vec![],
        };
        let current = Version::new(0, 8, 2);
        let latest = Version::parse_release_tag(&release.tag_name).unwrap();
        let check = UpdateCheck {
            current_version: current,
            latest_version: latest,
            has_update: latest > current,
            platform: None,
            asset: None,
            checksum_asset: None,
            install_method: InstallMethod::Unknown,
            latest_release: release,
        };
        assert!(check.has_update);
        assert!(!check.upgrade_command().is_empty());
    }

    fn release_at(tag: &str) -> Release {
        Release {
            tag_name: tag.to_string(),
            html_url: "https://example.com".to_string(),
            published_at: "2024-06-01T00:00:00Z".to_string(),
            assets: vec![],
        }
    }

    fn check_with(current: Version, tag: &str, method: InstallMethod) -> UpdateCheck {
        let release = release_at(tag);
        let latest = Version::parse_release_tag(tag).unwrap();
        UpdateCheck {
            current_version: current,
            latest_version: latest,
            has_update: latest > current,
            platform: None,
            asset: None,
            checksum_asset: None,
            install_method: method,
            latest_release: release,
        }
    }

    #[test]
    fn update_check_no_update_when_current_is_newer() {
        let check = check_with(Version::new(1, 0, 0), "v0.9.0", InstallMethod::Brew);
        assert!(!check.has_update);
    }

    #[test]
    fn update_check_equal_version_is_not_an_update() {
        let check = check_with(Version::new(0, 9, 0), "v0.9.0", InstallMethod::Cargo);
        assert!(!check.has_update);
    }

    #[test]
    fn update_check_command_delegates_to_install_method() {
        let brew = check_with(Version::new(0, 1, 0), "v0.9.0", InstallMethod::Brew);
        assert_eq!(brew.upgrade_command(), InstallMethod::Brew.upgrade_command());
        assert_eq!(brew.upgrade_description(), InstallMethod::Brew.description());

        let docker = check_with(Version::new(0, 1, 0), "v0.9.0", InstallMethod::Docker);
        assert_eq!(docker.upgrade_command(), InstallMethod::Docker.upgrade_command());
    }
}
