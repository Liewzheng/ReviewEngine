#!/usr/bin/env bash
#
# Publish review-engine generic package assets and a GitLab release.
#
# Usage:
#   ./scripts/publish-release.sh daily
#   ./scripts/publish-release.sh stable v0.1.0
#

set -euo pipefail

MODE="${1:-}"
VERSION="${2:-}"

if ! command -v jq &>/dev/null; then
    echo "ERROR: jq is required but not installed." >&2
    echo "Please install jq on the runner before running this job." >&2
    exit 1
fi

if [ -z "${CI_PROJECT_ID:-}" ]; then
    echo "ERROR: CI_PROJECT_ID is not set." >&2
    exit 1
fi

if [ -z "${CI_JOB_TOKEN:-}" ]; then
    echo "ERROR: CI_JOB_TOKEN is not set." >&2
    exit 1
fi

if [ -z "${CI_PROJECT_URL:-}" ]; then
    echo "ERROR: CI_PROJECT_URL is not set." >&2
    exit 1
fi

if [ -z "${CI_API_V4_URL:-}" ]; then
    echo "ERROR: CI_API_V4_URL is not set." >&2
    exit 1
fi

if [ -z "${CI_COMMIT_SHA:-}" ]; then
    echo "ERROR: CI_COMMIT_SHA is not set." >&2
    exit 1
fi

AUTH_HEADER="JOB-TOKEN: ${CI_JOB_TOKEN}"

ASSETS=(
    "target/release/review-engine:review-engine-aarch64-linux"
    "target/x86_64-unknown-linux-gnu/release/review-engine:review-engine-x86_64-linux"
    "target/x86_64-pc-windows-gnu/release/review-engine.exe:review-engine-x86_64-windows.exe"
    "target/aarch64-apple-darwin/release/review-engine:review-engine-aarch64-apple-darwin"
    "target/x86_64-apple-darwin/release/review-engine:review-engine-x86_64-apple-darwin"
)

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

upload_asset() {
    local local_path="$1"
    local filename="$2"
    local package_version="$3"

    if [ ! -f "${local_path}" ]; then
        echo "ERROR: asset file not found: ${local_path}" >&2
        exit 1
    fi

    local upload_url="${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/packages/generic/review-engine/${package_version}/${filename}?select=package_file"

    echo "Uploading ${filename} ..." >&2
    local response
    response=$(curl --fail --silent --show-error \
        --header "${AUTH_HEADER}" \
        -T "${local_path}" \
        "${upload_url}")

    echo "Uploaded ${filename}" >&2
    echo "${response}"
}

upload_with_checksum() {
    local local_path="$1"
    local filename="$2"
    local package_version="$3"
    local checksum_path="${local_path}.sha256"

    sha256sum "${local_path}" > "${checksum_path}"

    local asset_response checksum_response
    asset_response=$(upload_asset "${local_path}" "${filename}" "${package_version}")
    checksum_response=$(upload_asset "${checksum_path}" "${filename}.sha256" "${package_version}")

    # Emit JSON objects, one per line, to be parsed by the caller.
    printf '%s\n' "${asset_response}"
    printf '%s\n' "${checksum_response}"
}

build_links_from_upload_responses() {
    local responses="$1"

    echo "${responses}" | jq -c -s '
        [ .[] |
          select(.package_id != null and .id != null and .file_name != null) |
          { name: .file_name, url: (env.CI_PROJECT_URL + "/-/package_files/" + (.id | tostring) + "/download"), link_type: "package" }
        ]
    '
}

release_exists() {
    local tag_name="$1"
    local url="${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/releases/${tag_name}"
    local status

    status=$(curl --silent --show-error --output /dev/null --write-out '%{http_code}' --header "${AUTH_HEADER}" "${url}")
    [ "${status}" = "200" ]
}

create_or_update_release() {
    local tag_name="$1"
    local name="$2"
    local asset_links="$3"
    local body="${4:-}"

    local payload
    payload=$(jq -n \
        --arg tag_name "${tag_name}" \
        --arg name "${name}" \
        --arg body "${body}" \
        --argjson assets "${asset_links}" \
        '{tag_name: $tag_name, name: $name, description: $body, assets: {links: $assets}}')

    if release_exists "${tag_name}"; then
        echo "Updating existing release for ${tag_name} ..."
        curl --fail --silent --show-error \
            --request PUT \
            --header "${AUTH_HEADER}" \
            --header "Content-Type: application/json" \
            --data "${payload}" \
            "${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/releases/${tag_name}"
    else
        echo "Creating new release for ${tag_name} ..."
        curl --fail --silent --show-error \
            --request POST \
            --header "${AUTH_HEADER}" \
            --header "Content-Type: application/json" \
            --data "${payload}" \
            "${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/releases"
    fi
    echo "Release ${tag_name} published."
}

delete_tag() {
    local tag_name="$1"
    local url="${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/repository/tags/${tag_name}"
    local status

    status=$(curl --silent --show-error --output /dev/null --write-out '%{http_code}' --request DELETE --header "${AUTH_HEADER}" "${url}" || true)
    if [ "${status}" = "204" ] || [ "${status}" = "200" ]; then
        echo "Deleted existing tag ${tag_name}."
    elif [ "${status}" = "404" ]; then
        echo "Tag ${tag_name} did not exist."
    else
        echo "Ignoring tag deletion status: ${status}"
    fi
}

create_lightweight_tag() {
    local tag_name="$1"
    local ref="$2"

    echo "Creating lightweight tag ${tag_name} at ${ref} ..."
    curl --fail --silent --show-error \
        --request POST \
        --header "${AUTH_HEADER}" \
        --data-urlencode "tag_name=${tag_name}" \
        --data-urlencode "ref=${ref}" \
        --data-urlencode "message=Release ${tag_name}" \
        "${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/repository/tags"
    echo "Tag ${tag_name} created."
}

delete_package_by_id() {
    local package_id="$1"
    local url="${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/packages/${package_id}"

    echo "Deleting old daily package ${package_id} ..."
    curl --fail --silent --show-error \
        --request DELETE \
        --header "${AUTH_HEADER}" \
        "${url}" || true
    echo "Deleted old daily package ${package_id}"
}

cleanup_old_daily_packages() {
    local keep_count="$1"
    local url="${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/packages?package_name=review-engine&per_page=100"
    local response

    response=$(curl --fail --silent --show-error --header "${AUTH_HEADER}" "${url}")

    local to_delete
    to_delete=$(echo "${response}" | jq -r '
        [ .[] | select(.version | startswith("daily-")) ]
        | sort_by(.created_at) | reverse
        | .['"${keep_count}"':] []
        | .id
    ')

    local package_id
    while IFS= read -r package_id; do
        [ -z "${package_id}" ] && continue
        delete_package_by_id "${package_id}"
    done <<< "${to_delete}"
}

# ---------------------------------------------------------------------------
# Main modes
# ---------------------------------------------------------------------------

ASSET_LINKS="[]"

case "${MODE}" in
    daily)
        DAILY_VERSION="daily-$(date -u +%Y%m%d)-${CI_COMMIT_SHORT_SHA:-unknown}"
        VERSION="${DAILY_VERSION}"

        echo "Publishing daily release (version=${VERSION}) ..."

        # Upload assets and their checksums, capturing upload responses.
        UPLOAD_RESPONSES=""
        for asset in "${ASSETS[@]}"; do
            IFS=':' read -r local_path filename <<< "${asset}"
            UPLOAD_RESPONSES+="$(upload_with_checksum "${local_path}" "${filename}" "${VERSION}")"$'\n'
        done

        ASSET_LINKS=$(build_links_from_upload_responses "${UPLOAD_RESPONSES}")

        # Update the daily-build tag to point to the current commit.
        delete_tag "daily-build"
        create_lightweight_tag "daily-build" "${CI_COMMIT_SHA}"

        # Create or update the daily release.
        create_or_update_release \
            "daily-build" \
            "Daily Build" \
            "${ASSET_LINKS}" \
            "Automated daily build ${VERSION} from ${CI_COMMIT_SHA}."

        # Keep only the latest 7 daily packages.
        cleanup_old_daily_packages 7
        ;;

    stable)
        if [ -z "${VERSION}" ]; then
            echo "ERROR: version argument is required for stable mode." >&2
            echo "Usage: $0 stable v0.1.0" >&2
            exit 1
        fi

        echo "Publishing stable release (version=${VERSION}) ..."

        UPLOAD_RESPONSES=""
        for asset in "${ASSETS[@]}"; do
            IFS=':' read -r local_path filename <<< "${asset}"
            UPLOAD_RESPONSES+="$(upload_with_checksum "${local_path}" "${filename}" "${VERSION}")"$'\n'
        done

        ASSET_LINKS=$(build_links_from_upload_responses "${UPLOAD_RESPONSES}")

        # Create or update the stable release.
        create_or_update_release \
            "${VERSION}" \
            "${VERSION}" \
            "${ASSET_LINKS}" \
            "Stable release ${VERSION}."
        ;;

    *)
        echo "ERROR: unknown mode '${MODE}'" >&2
        echo "Usage: $0 daily" >&2
        echo "       $0 stable <version>" >&2
        exit 1
        ;;
esac

echo "Done."
