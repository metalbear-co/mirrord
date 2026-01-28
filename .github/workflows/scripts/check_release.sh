#!/bin/bash
set -euo pipefail

# GitHub Actions shell script to check for release assets and Docker images.
#
# Expectations:
# - CHECK_TYPE: 'mirrord' or 'operator'
# - OPERATOR_REPO_TOKEN (optional, for operator checks)
# - GITHUB_TOKEN (required for API calls)
# - GITHUB_REPOSITORY (e.g., metalbear-co/mirrord)

check_mirrord_release() {
    local owner_repo="$GITHUB_REPOSITORY"
    echo "Checking latest release for $owner_repo..."

    local release_data
    release_data=$(curl -s -H "Authorization: token $GITHUB_TOKEN" "https://api.github.com/repos/$owner_repo/releases/latest")
    
    local tag
    tag=$(echo "$release_data" | jq -r .tag_name)
    if [ "$tag" == "null" ] || [ -z "$tag" ]; then
        echo "::warning::Failed to get latest release tag."
        echo "API Response: $release_data"
        echo "ok=false" >> "$GITHUB_OUTPUT"
        echo "error=Failed to get latest release tag" >> "$GITHUB_OUTPUT"
        return 1
    fi

    local html_url
    html_url=$(echo "$release_data" | jq -r .html_url)
    
    local asset_names
    asset_names=$(echo "$release_data" | jq -r '.assets[].name')

    echo "Latest release tag: $tag"
    # echo "Assets: $asset_names"

    # REQUIRED ASSETS
    local required_assets=(
        "mirrord_linux_x86_64.zip"
        "mirrord_linux_x86_64.shasum256"
        "mirrord_linux_x86_64"
        "libmirrord_layer_linux_x86_64.so"
        "mirrord_linux_aarch64.zip"
        "mirrord_linux_aarch64.shasum256"
        "mirrord_linux_aarch64"
        "libmirrord_layer_linux_aarch64.so"
        "mirrord_mac_universal.zip"
        "mirrord_mac_universal.shasum256"
        "mirrord_mac_universal"
        "libmirrord_layer_mac_universal.dylib"
    )

    local missing_required=()
    for asset in "${required_assets[@]}"; do
        if ! echo "$asset_names" | grep -q "^$asset$"; then
            missing_required+=("$asset")
        fi
    done

    # Docker image check
    local mirrord_image="ghcr.io/$owner_repo:$tag"
    echo "Checking for Mirrord Docker image: $mirrord_image"
    if ! docker manifest inspect "$mirrord_image" > /dev/null 2>&1; then
        echo "::warning::Failed to find Mirrord Docker image $mirrord_image"
        missing_required+=("Docker Image ($mirrord_image)")
    fi

    local optional_assets=(
        "mirrord.exe"
        "mirrord.exe.sha256"
        "mirrord_layer_win.dll"
        "mirrord_layer_win.dll.sha256"
    )
    local missing_optional=()
    for asset in "${optional_assets[@]}"; do
        if ! echo "$asset_names" | grep -q "^$asset$"; then
            missing_optional+=("$asset")
        fi
    done

    echo "tag=$tag" >> "$GITHUB_OUTPUT"
    echo "html_url=$html_url" >> "$GITHUB_OUTPUT"
    
    local missing_req_str
    missing_req_str=$(IFS=, ; echo "${missing_required[*]}")
    echo "missing_required=$missing_req_str" >> "$GITHUB_OUTPUT"
    
    local missing_opt_str
    missing_opt_str=$(IFS=, ; echo "${missing_optional[*]}")
    echo "missing_optional=$missing_opt_str" >> "$GITHUB_OUTPUT"

    if [ ${#missing_required[@]} -gt 0 ]; then
        local msg="Missing REQUIRED assets on latest release $tag: $missing_req_str"
        echo "::warning::$msg"
        echo "ok=false" >> "$GITHUB_OUTPUT"
        echo "error=$msg" >> "$GITHUB_OUTPUT"
    else
        echo "All required assets are present on latest release."
        if [ ${#missing_optional[@]} -gt 0 ]; then
            echo "::warning::Missing OPTIONAL (Windows) assets on latest release $tag: $missing_opt_str"
        fi
        echo "ok=true" >> "$GITHUB_OUTPUT"
        echo "error=" >> "$GITHUB_OUTPUT"
    fi
}

check_operator_release() {
    local operator_repo="metalbear-co/operator"
    local token="${OPERATOR_REPO_TOKEN:-$GITHUB_TOKEN}"

    echo "Fetching latest Operator version from GitHub API for $operator_repo..."
    local release_data
    release_data=$(curl -s -H "Authorization: token $token" "https://api.github.com/repos/$operator_repo/releases/latest")
    
    local tag
    tag=$(echo "$release_data" | jq -r .tag_name)
    if [ "$tag" == "null" ] || [ -z "$tag" ]; then
        echo "::warning::Failed to get latest operator version."
        echo "API Response: $release_data"
        echo "ok=false" >> "$GITHUB_OUTPUT"
        echo "error=Failed to get latest operator version" >> "$GITHUB_OUTPUT"
        return 1
    fi

    echo "Latest Operator release tag from GitHub: $tag"
    echo "tag=$tag" >> "$GITHUB_OUTPUT"

    local operator_image="ghcr.io/metalbear-co/operator:$tag"
    echo "Checking for Operator Docker image: $operator_image"
    if ! docker manifest inspect "$operator_image" > /dev/null 2>&1; then
        local msg="Failed to find Operator Docker image $operator_image"
        echo "::warning::$msg"
        echo "ok=false" >> "$GITHUB_OUTPUT"
        echo "error=$msg" >> "$GITHUB_OUTPUT"
    else
        echo "✅ Operator Docker image found: $operator_image"
        echo "ok=true" >> "$GITHUB_OUTPUT"
        echo "error=" >> "$GITHUB_OUTPUT"
    fi

    # Check GitHub Release Page
    local release_url="https://github.com/$operator_repo/releases/tag/$tag"
    echo "Checking Operator Release Page: $release_url"
    if curl -fsI "$release_url" > /dev/null; then
        echo "✅ Operator Release Page is reachable."
    else
        echo "::warning::Operator Release Page unreachable (likely private): $release_url"
    fi

    # Check Source Code Zip
    local asset_url="https://github.com/$operator_repo/archive/refs/tags/$tag.zip"
    echo "Checking Operator Source Code Zip: $asset_url"
    if curl -fsI -H "Authorization: token $token" "$asset_url" > /dev/null; then
        echo "✅ Operator Source Code Zip found."
    else
        local msg="Operator Source Code Zip unreachable (Auth failure?): $asset_url"
        echo "::warning::$msg"
        echo "ok=false" >> "$GITHUB_OUTPUT"
        # Append to error if already exists
        local current_error
        # We can't easily read back output from $GITHUB_OUTPUT in shell reliably without a temp file or parsing
        # but for simplicity we just overwrite or append if we had a mechanism. 
        # Since we just set ok=false above, this will just reinforce it.
    fi
}

case "${CHECK_TYPE:-}" in
    mirrord)
        check_mirrord_release
        ;;
    operator)
        check_operator_release
        ;;
    *)
        echo "Error: Unknown CHECK_TYPE: ${CHECK_TYPE:-}"
        exit 1
        ;;
esac
