//! Detect how review-engine was installed so upgrade instructions can match.
//!
//! This mapping is shared by the CLI upgrade prompt and the web status API —
//! it is the single source of truth for "what command do I tell the user".

use std::path::Path;

/// How the running binary appears to have been installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    /// Homebrew (`/opt/homebrew/...` or `/usr/local/Cellar/...`).
    Brew,
    /// `cargo install` (`~/.cargo/bin/...`).
    Cargo,
    /// Running inside a container (`/.dockerenv` present) — the binary and
    /// frontend dist live on writable volumes, so upgrades run in place and
    /// the container restarts itself.
    Docker,
    /// A plain copy in a bin directory (`~/.local/bin`, `/usr/local/bin`, ...).
    Plain,
    /// Could not be determined.
    Unknown,
}

impl InstallMethod {
    /// Detect the install method for the current process.
    ///
    /// `current_exe` is canonicalized first so a Homebrew symlink
    /// (`/usr/local/bin/review-engine`) resolves to its real
    /// (`/usr/local/Cellar/.../bin/review-engine`) target before matching.
    pub fn detect() -> Self {
        let exe = std::env::current_exe().ok();
        let canonical = exe.as_ref().and_then(|p| std::fs::canonicalize(p).ok());
        let path = canonical.as_deref().or(exe.as_deref());
        Self::detect_from(path, Path::new("/.dockerenv").exists())
    }

    /// Detect from an explicit executable path (testable) plus a
    /// "running inside Docker" flag.
    ///
    /// Match order is deliberate: Docker first (it wins over everything), then
    /// Brew (checked before Plain because brew symlinks live in
    /// `/usr/local/bin`, which is also a Plain path).
    pub fn detect_from(exe: Option<&Path>, in_docker: bool) -> Self {
        if in_docker {
            return Self::Docker;
        }
        let Some(path) = exe else {
            return Self::Unknown;
        };
        // Normalize separators so the same rules work on Windows too.
        let p = path.to_string_lossy().replace('\\', "/");
        if p.contains("/Cellar/") || p.contains("/opt/homebrew/") {
            Self::Brew
        } else if p.contains("/.cargo/bin/") {
            Self::Cargo
        } else if p.contains("/.local/bin/") || p.contains("/usr/local/bin/") {
            Self::Plain
        } else {
            Self::Unknown
        }
    }

    /// One-line upgrade command for the detected install method.
    pub fn upgrade_command(self) -> &'static str {
        match self {
            Self::Brew => "brew upgrade review-engine",
            Self::Cargo => "cargo install review-engine --locked --features cli",
            // In-container auto-upgrade via the Web UI (`/api/v1/system/upgrade`)
            // or the `reng upgrade` CLI; the container restarts itself on
            // completion.
            Self::Docker => "Web UI 或 reng upgrade 自动升级（容器将自动重启）",
            Self::Plain => "reng upgrade",
            Self::Unknown => "使用官方 install.sh 手动升级",
        }
    }

    /// Longer human-readable upgrade explanation.
    pub fn description(self) -> &'static str {
        match self {
            Self::Brew => "检测到 Homebrew 安装：请运行 `brew upgrade review-engine`。",
            Self::Cargo => "检测到 cargo 安装：请重新安装 `cargo install review-engine --locked --features cli`。",
            Self::Docker => "检测到容器环境：可使用 Web UI 或 `reng upgrade` 在容器内自动升级，完成后容器将自动重启。",
            Self::Plain => "检测到直接部署的二进制：请使用内置命令 `reng upgrade` 完成升级。",
            Self::Unknown => "无法识别安装方式：请使用官方 install.sh 手动升级（见 GitHub Releases 页面）。",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Option<&Path> {
        Some(Path::new(s))
    }

    #[test]
    fn detects_brew() {
        assert_eq!(
            InstallMethod::detect_from(p("/opt/homebrew/bin/review-engine"), false),
            InstallMethod::Brew
        );
        assert_eq!(
            InstallMethod::detect_from(p("/opt/homebrew/Cellar/review-engine/0.8.2/bin/review-engine"), false),
            InstallMethod::Brew
        );
        assert_eq!(
            InstallMethod::detect_from(p("/usr/local/Cellar/review-engine/0.8.2/bin/review-engine"), false),
            InstallMethod::Brew
        );
    }

    #[test]
    fn detects_cargo() {
        assert_eq!(
            InstallMethod::detect_from(p("/Users/x/.cargo/bin/review-engine"), false),
            InstallMethod::Cargo
        );
        assert_eq!(
            InstallMethod::detect_from(p("/root/.cargo/bin/review-engine"), false),
            InstallMethod::Cargo
        );
        assert_eq!(
            InstallMethod::detect_from(p("C:\\Users\\x\\.cargo\\bin\\review-engine.exe"), false),
            InstallMethod::Cargo,
            "windows backslash path must also match"
        );
    }

    #[test]
    fn detects_plain() {
        assert_eq!(
            InstallMethod::detect_from(p("/Users/x/.local/bin/review-engine"), false),
            InstallMethod::Plain
        );
        assert_eq!(
            InstallMethod::detect_from(p("/usr/local/bin/review-engine"), false),
            InstallMethod::Plain
        );
    }

    #[test]
    fn docker_wins_over_path() {
        assert_eq!(
            InstallMethod::detect_from(p("/app/review-engine"), true),
            InstallMethod::Docker
        );
        assert_eq!(
            InstallMethod::detect_from(p("/opt/homebrew/bin/review-engine"), true),
            InstallMethod::Docker
        );
        assert_eq!(
            InstallMethod::detect_from(p("/usr/local/bin/review-engine"), true),
            InstallMethod::Docker
        );
    }

    #[test]
    fn unknown_cases() {
        assert_eq!(InstallMethod::detect_from(None, false), InstallMethod::Unknown);
        assert_eq!(
            InstallMethod::detect_from(p("/tmp/scratch/review-engine"), false),
            InstallMethod::Unknown
        );
        assert_eq!(
            InstallMethod::detect_from(p("review-engine"), false),
            InstallMethod::Unknown
        );
    }

    #[test]
    fn hints_match_design() {
        assert_eq!(InstallMethod::Brew.upgrade_command(), "brew upgrade review-engine");
        assert_eq!(
            InstallMethod::Cargo.upgrade_command(),
            "cargo install review-engine --locked --features cli"
        );
        assert_eq!(
            InstallMethod::Docker.upgrade_command(),
            "Web UI 或 reng upgrade 自动升级（容器将自动重启）"
        );
        assert_eq!(
            InstallMethod::Docker.description(),
            "检测到容器环境：可使用 Web UI 或 `reng upgrade` 在容器内自动升级，完成后容器将自动重启。"
        );
        assert_eq!(InstallMethod::Plain.upgrade_command(), "reng upgrade");
        for m in [
            InstallMethod::Brew,
            InstallMethod::Cargo,
            InstallMethod::Docker,
            InstallMethod::Plain,
            InstallMethod::Unknown,
        ] {
            assert!(!m.upgrade_command().is_empty(), "{m:?} must have a command");
            assert!(!m.description().is_empty(), "{m:?} must have a description");
        }
    }
}
