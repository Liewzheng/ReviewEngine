//! Maps `std::env::consts::{OS, ARCH}` onto release asset identities.
//!
//! A release publishes one archive per rustc target triple plus a matching
//! `<prefix>-<triple>.sha256` sidecar (published by
//! taiki-e/upload-rust-binary-action; the checksum name carries **no** archive
//! extension). This module owns that mapping and the asset naming rules so
//! download/verify and the CLI/Web layers agree on names.

use super::error::{Result, UpgradeError};

/// Archive container format for a release asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetFormat {
    /// `review-engine-<triple>.tar.gz`
    TarGz,
    /// `review-engine-<triple>.zip` (Windows)
    Zip,
}

/// Identity of the release asset for one platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetSpec {
    pub triple: &'static str,
    pub format: AssetFormat,
}

impl AssetSpec {
    /// Asset file name, e.g. `review-engine-x86_64-apple-darwin.tar.gz`.
    pub fn asset_name(&self, prefix: &str) -> String {
        let ext = match self.format {
            AssetFormat::TarGz => "tar.gz",
            AssetFormat::Zip => "zip",
        };
        format!("{prefix}-{}.{ext}", self.triple)
    }

    /// Checksum sidecar name published by taiki-e/upload-rust-binary-action:
    /// `<prefix>-<triple>.sha256` — no archive extension (e.g.
    /// `review-engine-x86_64-apple-darwin.sha256`).
    pub fn checksum_name(&self, prefix: &str) -> String {
        format!("{prefix}-{}.sha256", self.triple)
    }

    /// `true` for the Windows zip format.
    pub fn is_windows(&self) -> bool {
        matches!(self.format, AssetFormat::Zip)
    }
}

/// Map an explicit `(os, arch)` pair to an asset spec.
///
/// `arch` accepts both `aarch64` (`std::env::consts::ARCH` on Apple Silicon
/// and arm64 Linux) and `arm64` as an alias.
pub fn asset_spec_for(os: &str, arch: &str) -> Result<AssetSpec> {
    let triple = match (os, arch) {
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64" | "arm64") => "aarch64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64" | "arm64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64" | "arm64") => "aarch64-pc-windows-msvc",
        _ => {
            return Err(UpgradeError::unsupported_platform(format!(
                "no release asset for OS={os:?}, ARCH={arch:?}"
            )));
        }
    };
    let format = if os == "windows" {
        AssetFormat::Zip
    } else {
        AssetFormat::TarGz
    };
    Ok(AssetSpec { triple, format })
}

/// Asset spec for the current process platform.
pub fn current_asset_spec() -> Result<AssetSpec> {
    asset_spec_for(std::env::consts::OS, std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_all_supported_platforms() {
        assert_eq!(asset_spec_for("macos", "x86_64").unwrap().triple, "x86_64-apple-darwin");
        assert_eq!(
            asset_spec_for("macos", "aarch64").unwrap().triple,
            "aarch64-apple-darwin"
        );
        assert_eq!(
            asset_spec_for("linux", "x86_64").unwrap().triple,
            "x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            asset_spec_for("linux", "aarch64").unwrap().triple,
            "aarch64-unknown-linux-gnu"
        );
        assert_eq!(
            asset_spec_for("windows", "x86_64").unwrap().triple,
            "x86_64-pc-windows-msvc"
        );
        assert_eq!(
            asset_spec_for("windows", "aarch64").unwrap().triple,
            "aarch64-pc-windows-msvc"
        );
    }

    #[test]
    fn arm64_is_an_alias_for_aarch64() {
        assert_eq!(asset_spec_for("macos", "arm64").unwrap().triple, "aarch64-apple-darwin");
        assert_eq!(
            asset_spec_for("linux", "arm64").unwrap().triple,
            "aarch64-unknown-linux-gnu"
        );
        assert_eq!(
            asset_spec_for("windows", "arm64").unwrap().triple,
            "aarch64-pc-windows-msvc"
        );
        assert_eq!(asset_spec_for("macos", "arm64").unwrap().format, AssetFormat::TarGz);
    }

    #[test]
    fn windows_uses_zip_everywhere_else_uses_tar_gz() {
        assert_eq!(asset_spec_for("windows", "x86_64").unwrap().format, AssetFormat::Zip);
        assert_eq!(asset_spec_for("windows", "aarch64").unwrap().format, AssetFormat::Zip);
        assert_eq!(asset_spec_for("linux", "x86_64").unwrap().format, AssetFormat::TarGz);
        assert_eq!(asset_spec_for("macos", "aarch64").unwrap().format, AssetFormat::TarGz);
    }

    #[test]
    fn rejects_unknown_platforms() {
        assert!(asset_spec_for("freebsd", "x86_64").is_err());
        assert!(asset_spec_for("macos", "riscv64").is_err());
        assert!(asset_spec_for("linux", "s390x").is_err());
        assert!(asset_spec_for("windows", "i686").is_err());
    }

    #[test]
    fn asset_and_checksum_names() {
        let mac = asset_spec_for("macos", "aarch64").unwrap();
        assert_eq!(
            mac.asset_name("review-engine"),
            "review-engine-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            mac.checksum_name("review-engine"),
            "review-engine-aarch64-apple-darwin.sha256"
        );

        let win = asset_spec_for("windows", "x86_64").unwrap();
        assert_eq!(
            win.asset_name("review-engine"),
            "review-engine-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(
            win.checksum_name("review-engine"),
            "review-engine-x86_64-pc-windows-msvc.sha256"
        );
        assert!(win.is_windows());
        assert!(!mac.is_windows());
    }

    #[test]
    fn unsupported_platform_is_a_hard_error() {
        assert!(asset_spec_for("freebsd", "x86_64").is_err());
        assert!(asset_spec_for("macos", "riscv64").is_err());
        assert!(asset_spec_for("linux", "s390x").is_err());
        let err = asset_spec_for("plan9", "x86_64").unwrap_err().to_string();
        assert!(
            err.contains("no release asset"),
            "error should name the platform, got: {err}"
        );
    }

    #[test]
    fn asset_name_uses_prefix_triple_and_archive_ext() {
        let spec = asset_spec_for("linux", "x86_64").unwrap();
        assert_eq!(
            spec.asset_name("review-engine"),
            "review-engine-x86_64-unknown-linux-gnu.tar.gz"
        );
        let win = asset_spec_for("windows", "x86_64").unwrap();
        assert_eq!(
            win.asset_name("review-engine"),
            "review-engine-x86_64-pc-windows-msvc.zip"
        );
    }

    #[test]
    fn checksum_name_has_no_archive_extension() {
        let spec = asset_spec_for("macos", "aarch64").unwrap();
        assert_eq!(
            spec.checksum_name("review-engine"),
            "review-engine-aarch64-apple-darwin.sha256"
        );
        // Checksum sidecar must NOT carry the .tar.gz / .zip archive extension.
        assert!(!spec.checksum_name("review-engine").contains("tar.gz"));
        assert!(!spec.checksum_name("review-engine").contains(".zip"));
    }

    #[test]
    fn is_windows_reflects_the_archive_format() {
        assert!(asset_spec_for("windows", "x86_64").unwrap().is_windows());
        assert!(asset_spec_for("windows", "aarch64").unwrap().is_windows());
        assert!(!asset_spec_for("linux", "x86_64").unwrap().is_windows());
        assert!(!asset_spec_for("macos", "aarch64").unwrap().is_windows());
    }

    #[test]
    fn asset_spec_is_copy_and_equatable() {
        let a = asset_spec_for("linux", "x86_64").unwrap();
        let b = a;
        assert_eq!(a, b, "Copy + PartialEq");
        let c = asset_spec_for("macos", "x86_64").unwrap();
        assert_ne!(a, c);
    }
}
