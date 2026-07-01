#!/usr/bin/env bash
#
# review-engine installer
# ========================
# This script installs the review-engine CLI tool to ~/.local/bin.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Liewzheng/Review-Engine/master/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/Liewzheng/Review-Engine/master/install.sh | bash -s -- --source
#
# Requirements for binary install: curl, jq
# Requirements for source install: git, cargo (rustup)
# License: Apache-2.0

set -euo pipefail

# ---------------------------------------------------------------------------
# ANSI colours & helpers
# ---------------------------------------------------------------------------
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m' # No Colour

info()    { printf "${BLUE}ℹ️  %s${NC}\n" "$*"; }
success() { printf "${GREEN}✅ %s${NC}\n" "$*"; }
warn()    { printf "${YELLOW}⚠️  %s${NC}\n" "$*"; }
error()   { printf "${RED}❌ %s${NC}\n" "$*"; }
header()  { printf "\n${BOLD}━━━ %s ━━━${NC}\n" "$*"; }

usage() {
    cat <<EOF
Usage: install.sh [OPTION]

Options:
  -h, --help        Show this help message and exit
      --source      Build and install from source (requires git and cargo)

Environment variables:
  GITHUB_TOKEN      Personal access token for private GitHub repositories (optional for public repos)
  REVIEW_ENGINE_VERSION  Override the stable version to install (e.g. v0.3.0)

Examples:
  install.sh                              # install latest stable binary
  install.sh --source                     # build and install from source
EOF
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
SOURCE_INSTALL=false

while [ $# -gt 0 ]; do
    case "$1" in
        --help|-h)
            usage
            exit 0
            ;;
        --source)
            SOURCE_INSTALL=true
            shift
            ;;
        *)
            error "Unknown argument: $1"
            usage >&2
            exit 1
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Shared configuration
# ---------------------------------------------------------------------------
GITHUB_OWNER="Liewzheng"
GITHUB_REPO="Review-Engine"
GITHUB_API="https://api.github.com/repos/${GITHUB_OWNER}/${GITHUB_REPO}"
REPO_URL="https://github.com/${GITHUB_OWNER}/${GITHUB_REPO}.git"
RAW_URL="https://raw.githubusercontent.com/${GITHUB_OWNER}/${GITHUB_REPO}"
BIN_DIR="${HOME}/.local/bin"
CONFIG_DIR="${HOME}/.config/review-engine"
BIN_PATH="${BIN_DIR}/review-engine"
DEFAULT_CONFIG="${CONFIG_DIR}/.code-audit-config.toml"

# Temporary working directory (cleaned up on exit)
TMP_DIR="$(mktemp -d -t review-engine-XXXXXX)"
trap 'rm -rf "$TMP_DIR"' EXIT INT TERM

# ---------------------------------------------------------------------------
# Source install path (original behaviour)
# ---------------------------------------------------------------------------
install_from_source() {
    header "Checking prerequisites"

    if ! command -v git &>/dev/null; then
        error "git is not installed. Please install git first."
        info "  macOS: brew install git"
        info "  Linux: apt install git / yum install git"
        exit 1
    fi
    success "git found: $(git --version 2>&1 | head -1)"

    if ! command -v cargo &>/dev/null; then
        if command -v rustup &>/dev/null; then
            info "cargo not in PATH, but rustup is available — trying rustup default toolchain"
            rustup default stable &>/dev/null || true
            if ! command -v cargo &>/dev/null; then
                error "cargo still not available after rustup default. Check your Rust installation."
                exit 1
            fi
        else
            error "cargo (Rust toolchain) is not installed."
            info "Install via: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
            info "Then restart your shell and re-run this script."
            exit 1
        fi
    fi
    success "cargo found: $(cargo --version 2>&1 | head -1)"

    header "Cloning repository"
    info "Cloning ${REPO_URL} ..."
    git clone --depth=1 "${REPO_URL}" "${TMP_DIR}/review-engine"
    success "Repository cloned"

    header "Building review-engine (release, features=cli)"
    cd "${TMP_DIR}/review-engine"
    cargo build --locked --release --features cli
    success "Build completed"

    header "Installing binary"
    if [ ! -d "${BIN_DIR}" ]; then
        mkdir -p "${BIN_DIR}"
        info "Created ${BIN_DIR}"
    fi

    cp "target/release/review-engine" "${BIN_PATH}"
    chmod +x "${BIN_PATH}"
    success "Installed review-engine to ${BIN_PATH}"

    if [ ! -d "${CONFIG_DIR}" ]; then
        mkdir -p "${CONFIG_DIR}"
        info "Created config directory ${CONFIG_DIR}"
    fi

    if [ -f "${DEFAULT_CONFIG}" ]; then
        info "Config file already exists at ${DEFAULT_CONFIG} — skipping"
    else
        cp "docs/code-audit-default.toml" "${DEFAULT_CONFIG}"
        success "Default config copied to ${DEFAULT_CONFIG}"
    fi
}

# ---------------------------------------------------------------------------
# Binary install helpers
# ---------------------------------------------------------------------------
detect_asset() {
    local arch
    local kernel
    arch="$(uname -m)"
    kernel="$(uname -s)"

    case "${arch}" in
        aarch64|arm64)
            case "${kernel}" in
                Linux)
                    echo "review-engine-aarch64-unknown-linux-gnu tar.gz review-engine"
                    ;;
                Darwin)
                    echo "review-engine-aarch64-apple-darwin tar.gz review-engine"
                    ;;
                MINGW*|Windows*|CYGWIN*)
                    echo "review-engine-aarch64-pc-windows-msvc zip review-engine.exe"
                    ;;
                *)
                    error "Unsupported platform: ${arch} ${kernel}"
                    info "Binary install is not yet available for this platform. Use --source to build from source."
                    exit 1
                    ;;
            esac
            ;;
        x86_64|amd64)
            case "${kernel}" in
                Linux)
                    echo "review-engine-x86_64-unknown-linux-gnu tar.gz review-engine"
                    ;;
                Darwin)
                    echo "review-engine-x86_64-apple-darwin tar.gz review-engine"
                    ;;
                MINGW*|Windows*|CYGWIN*)
                    echo "review-engine-x86_64-pc-windows-msvc zip review-engine.exe"
                    ;;
                *)
                    error "Unsupported platform: ${arch} ${kernel}"
                    info "Binary install is not yet available for this platform. Use --source to build from source."
                    exit 1
                    ;;
            esac
            ;;
        *)
            error "Unsupported architecture: ${arch}"
            info "Binary install is not yet available for this architecture. Use --source to build from source."
            exit 1
            ;;
    esac
}

download_file_with_auth() {
    local url="$1"
    local output_path="$2"

    local auth_args=()
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        auth_args+=("--header" "Authorization: Bearer ${GITHUB_TOKEN}")
    fi

    local http_code curl_exit
    set +e
    http_code=$(curl --fail --silent --show-error --retry 3 --location \
        --write-out '%{http_code}' \
        ${auth_args[@]+"${auth_args[@]}"} \
        --output "${output_path}" \
        "${url}")
    curl_exit=$?
    set -e

    if [ ${curl_exit} -ne 0 ] || [ "${http_code}" != "200" ]; then
        error "Download failed: ${url} (HTTP ${http_code:-unknown}, curl exit ${curl_exit})."
        rm -f "${output_path}"
        return 1
    fi

    if [ ! -s "${output_path}" ]; then
        error "Downloaded file is empty: ${url}"
        rm -f "${output_path}"
        return 1
    fi

    return 0
}

print_token_help() {
    error "Download failed with authentication/authorization error."
    info "The GitHub API or release assets may require authentication for private repositories."
    info "Set a personal access token and re-run:"
    info ""
    info "   export GITHUB_TOKEN='<your-token>'"
    info ""
    info "Then re-run the install command."
}

verify_checksum() {
    local file_path="$1"
    local checksum_path="$2"

    if [ ! -f "${checksum_path}" ]; then
        error "Checksum file missing: ${checksum_path}"
        return 1
    fi

    local expected actual
    expected="$(awk '{print $1}' "${checksum_path}")"

    if command -v sha256sum &>/dev/null; then
        actual="$(sha256sum "${file_path}" | awk '{print $1}')"
    elif command -v shasum &>/dev/null; then
        actual="$(shasum -a 256 "${file_path}" | awk '{print $1}')"
    else
        error "Neither sha256sum nor shasum is available. Cannot verify checksum."
        return 1
    fi

    if [ "${expected}" != "${actual}" ]; then
        error "SHA256 checksum mismatch."
        error "  Expected: ${expected}"
        error "  Actual:   ${actual}"
        return 1
    fi

    success "SHA256 checksum verified"
    return 0
}

fetch_stable_release() {
    local tag="$1"
    local url="${GITHUB_API}/releases/tags/${tag}"
    local response_path="${TMP_DIR}/stable-release.json"

    if ! download_file_with_auth "${url}" "${response_path}"; then
        return 1
    fi

    echo "${response_path}"
}

resolve_stable_version() {
    if [ -n "${REVIEW_ENGINE_VERSION:-}" ]; then
        info "Using REVIEW_ENGINE_VERSION override: ${REVIEW_ENGINE_VERSION}"
        echo "${REVIEW_ENGINE_VERSION}"
        return 0
    fi

    local releases_path="${TMP_DIR}/releases.json"
    if ! download_file_with_auth "${GITHUB_API}/releases" "${releases_path}"; then
        return 1
    fi

    local latest
    latest=$(jq -r '[.[] | select(.tag_name | test("^v[0-9]+\\.[0-9]+\\.[0-9]+$"))] | sort_by(.published_at) | reverse | first | .tag_name' "${releases_path}")

    if [ -z "${latest}" ] || [ "${latest}" = "null" ]; then
        error "Could not find a stable release matching vX.Y.Z."
        return 1
    fi

    echo "${latest}"
}

pick_asset_link() {
    local release_path="$1"
    local asset_name="$2"

    jq -r --arg name "${asset_name}" '.assets[]? | select(.name == $name) | .browser_download_url' "${release_path}" | head -n1
}

sanitized_config_ref() {
    local ref="$1"

    if [[ "${ref}" == *".."* ]] || [[ "${ref}" == /* ]]; then
        error "Invalid ref for config URL: ${ref}"
        return 1
    fi

    echo "${ref}" | jq -sRr '@uri'
}

install_default_config() {
    local ref="$1"

    header "Installing default config"
    if [ ! -d "${CONFIG_DIR}" ]; then
        mkdir -p "${CONFIG_DIR}"
        info "Created config directory ${CONFIG_DIR}"
    fi

    if [ -f "${DEFAULT_CONFIG}" ]; then
        info "Config file already exists at ${DEFAULT_CONFIG} — skipping"
        return 0
    fi

    local encoded_ref
    encoded_ref=$(sanitized_config_ref "${ref}")

    local config_url="${RAW_URL}/${encoded_ref}/docs/code-audit-default.toml"
    info "Downloading default config from ${config_url} ..."

    if ! download_file_with_auth "${config_url}" "${DEFAULT_CONFIG}"; then
        error "Could not download default config. Aborting installation."
        return 1
    fi

    success "Default config copied to ${DEFAULT_CONFIG}"
}

install_binary() {
    header "Checking prerequisites"

    if ! command -v curl &>/dev/null; then
        error "curl is not installed. Please install curl first."
        info "  macOS: brew install curl"
        info "  Linux: apt install curl / yum install curl"
        exit 1
    fi
    success "curl found: $(curl --version 2>&1 | head -1)"

    if ! command -v jq &>/dev/null; then
        error "jq is not installed. It is required to resolve release assets."
        info "Install jq and re-run, or use --source to build from source."
        info "  macOS: brew install jq"
        info "  Debian/Ubuntu: sudo apt install jq"
        info "  Fedora/RHEL:   sudo dnf install jq"
        info "  Arch:          sudo pacman -S jq"
        exit 1
    fi
    success "jq found: $(jq --version 2>&1 | head -1)"

    if ! command -v sha256sum &>/dev/null && ! command -v shasum &>/dev/null; then
        error "Neither sha256sum nor shasum is installed. Please install coreutils or perl first."
        exit 1
    fi

    header "Detecting platform"
    local asset ext binary_name
    read -r asset ext binary_name <<< "$(detect_asset)"
    local archive_name="${asset}.${ext}"
    success "Detected asset: ${archive_name} (binary: ${binary_name})"

    header "Resolving version"
    local version
    local config_ref
    local release_path

    if ! version=$(resolve_stable_version); then
        exit 1
    fi
    config_ref="${version}"
    success "Latest stable version: ${version}"
    if ! release_path=$(fetch_stable_release "${version}"); then
        exit 1
    fi

    local asset_url checksum_url
    asset_url=$(pick_asset_link "${release_path}" "${archive_name}")
    checksum_url=$(pick_asset_link "${release_path}" "${archive_name}.sha256")

    if [ -z "${asset_url}" ]; then
        error "Could not find asset link for ${archive_name} in release."
        exit 1
    fi

    local tmp_archive="${TMP_DIR}/${archive_name}"
    local tmp_checksum="${TMP_DIR}/${archive_name}.sha256"
    local tmp_binary="${TMP_DIR}/${binary_name}"

    header "Downloading archive"
    info "URL: ${asset_url}"
    if ! download_file_with_auth "${asset_url}" "${tmp_archive}"; then
        exit 1
    fi
    success "Archive downloaded successfully"

    if [ -n "${checksum_url}" ]; then
        header "Downloading checksum"
        info "URL: ${checksum_url}"
        if download_file_with_auth "${checksum_url}" "${tmp_checksum}"; then
            header "Verifying checksum"
            if ! verify_checksum "${tmp_archive}" "${tmp_checksum}"; then
                exit 1
            fi
        else
            warn "Could not download checksum for ${archive_name}. Skipping verification."
        fi
    else
        warn "No checksum asset found for ${archive_name}. Skipping verification."
    fi

    header "Extracting binary"
    case "${ext}" in
        tar.gz)
            if ! tar -xzf "${tmp_archive}" -C "${TMP_DIR}"; then
                error "Failed to extract ${archive_name}"
                exit 1
            fi
            ;;
        zip)
            if ! command -v unzip &>/dev/null; then
                error "unzip is not installed. Please install unzip first."
                exit 1
            fi
            if ! unzip -q "${tmp_archive}" -d "${TMP_DIR}"; then
                error "Failed to extract ${archive_name}"
                exit 1
            fi
            ;;
        *)
            error "Unsupported archive extension: ${ext}"
            exit 1
            ;;
    esac

    if [ ! -f "${tmp_binary}" ]; then
        error "Extracted binary not found: ${binary_name}"
        exit 1
    fi
    success "Extracted ${binary_name}"

    header "Installing binary"
    if [ ! -d "${BIN_DIR}" ]; then
        mkdir -p "${BIN_DIR}"
        info "Created ${BIN_DIR}"
    fi

    cp "${tmp_binary}" "${BIN_PATH}"
    chmod +x "${BIN_PATH}"
    success "Installed review-engine to ${BIN_PATH}"

    if ! install_default_config "${config_ref}"; then
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# Main install flow
# ---------------------------------------------------------------------------
if [ "${SOURCE_INSTALL}" = true ]; then
    install_from_source
else
    install_binary
fi

# ---------------------------------------------------------------------------
# PATH detection
# ---------------------------------------------------------------------------
header "PATH setup"

update_path=false
if [[ ":$PATH:" != *":${BIN_DIR}:"* ]]; then
    warn "${BIN_DIR} is not in your PATH"
    echo ""
    echo "   Run the following command, or add it to your ${HOME}/.bashrc / ${HOME}/.zshrc:"
    echo ""
    printf "   ${BOLD}export PATH=\"\$HOME/.local/bin:\$PATH\"${NC}\n"
    echo ""
    # Update PATH for the current shell session
    export PATH="${BIN_DIR}:${PATH}"
    success "Updated PATH for the current shell session"
    update_path=true
else
    success "${BIN_DIR} is already in your PATH"
fi

# ---------------------------------------------------------------------------
# Detect shell and give hint
# ---------------------------------------------------------------------------
shell_hint=""
shell_name="$(basename "${SHELL:-unknown}" 2>/dev/null)"
case "${shell_name}" in
    bash) shell_hint="${HOME}/.bashrc" ;;
    zsh)  shell_hint="${HOME}/.zshrc"  ;;
    fish) shell_hint="${HOME}/.config/fish/config.fish" ;;
    *)    shell_hint="your shell profile" ;;
esac

if [ "${update_path}" = true ]; then
    echo ""
    info "To make this permanent, add the following line to ${shell_hint}:"
    echo ""
    printf "   ${BOLD}export PATH=\"\$HOME/.local/bin:\$PATH\"${NC}\n"
    echo ""
fi

# ---------------------------------------------------------------------------
# Verify installation
# ---------------------------------------------------------------------------
header "Verification"

if "${BIN_PATH}" --version &>/dev/null 2>&1; then
    version_output=$("${BIN_PATH}" --version 2>&1 | head -1)
    success "review-engine is working: ${version_output}"
elif "${BIN_PATH}" --help &>/dev/null 2>&1; then
    success "review-engine is installed (--version not available, but --help works)"
else
    warn "Binary installed but failed to run. You may need to check dependencies."
fi

# ---------------------------------------------------------------------------
# Cleanup (handled by trap) & success message
# ---------------------------------------------------------------------------
header "Installation complete"

echo ""
success "review-engine has been installed successfully! 🎉"
echo ""
echo "   Binary:       ${BIN_PATH}"
echo "   Config dir:   ${CONFIG_DIR}"
echo "   Config file:  ${DEFAULT_CONFIG}"
echo ""
echo "Quick start:"
echo ""
echo "   ${BOLD}review-engine --help${NC}"
echo "   ${BOLD}review-engine /review <PR_URL>${NC}"
echo ""
echo "Edit ${DEFAULT_CONFIG} to customise reviewers,"
echo "enable commands, or adjust weights."
echo ""
echo "Happy reviewing! 🚀"
echo ""
