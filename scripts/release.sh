#!/usr/bin/env bash

# release.sh
#
# This script automates the process of preparing a new release for mirrord.
#
# Usage:
#   ./release.sh 1.234.5
#
# Requirements:
# - `git`
# - `cargo-get` for querying information from a `Cargo.toml` file
# - `cargo-set` for setting workspace package version
# - `towncrier` for changelog generation
#
# What this script does:
# - Checkout a new branch and use the release version as the name of the branch.
# - Bump workspace version in `Cargo.toml`.
# - Prepare changelog using `towncrier`.
# - Push the branch to remote.
#
# Continue with the following steps after running this script:
# - Create a PR and merge it into mirrord main.
# - Create a new Github release with a new tag. This triggers the `Release` workflow.
#     - Use the new version for the tag and release name.
#     - Use the changelog as the release description excluding the `Internal` section.
# - Once the workflow finishes, adjust environment variables in our analytics server.
#

set -e

# Print usage and exit
usage() {
    echo "Usage: $0 <version>"
    echo "Example: $0 1.234.5"
    exit 1
}

# Check for version argument
VERSION="$1"
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Error: Invalid version format: '$VERSION'"
    usage
fi

# Ensure we are in the repo root
cd "$(git rev-parse --show-toplevel)"

# Ensure we're on the main branch or switch to it
CURRENT_BRANCH=$(git rev-parse --abbrev-ref HEAD)
if [[ "$CURRENT_BRANCH" != "main" ]]; then
    if [[ -n $(git status --porcelain) ]]; then
        echo "Error: You are on branch '$CURRENT_BRANCH' with uncommitted or unstaged changes. Please commit or stash them before switching to 'main'."
        exit 1
    else
        echo "You are on branch '$CURRENT_BRANCH'. Switching to 'main'..."
        git checkout main
    fi
fi

# Ensure main is up to date
git pull

# Get current version from workspace Cargo.toml
CURRENT_VERSION=$(cargo get workspace.package.version)
echo "Preparing release for version ${VERSION} (current: ${CURRENT_VERSION})"
read -rp "Do you want to continue? (y/n): " CONTINUE
case "$CONTINUE" in
    [Yy]* )
        ;;
    [Nn]* )
        echo "Aborting release preparation."
        exit 0
        ;;
    * )
        echo "Invalid input. Aborting."
        exit 1
        ;;
esac

# Create new branch
BRANCH_NAME="${VERSION}"
git checkout -b "$BRANCH_NAME"
echo "Created and switched to branch '$BRANCH_NAME'"

# Bump version in Cargo.toml
cargo set workspace.package.version "$VERSION"
# Refresh Cargo.lock to reflect version change
cargo update -w -q

# Build changelog
towncrier build --yes --version "${VERSION}"

# Stage all changes
git add Cargo.toml Cargo.lock changelog.d/ CHANGELOG.md

# Prompt user to push
read -rp "Do you want to push origin/${BRANCH_NAME}? (y/n): " CONFIRM
case "$CONFIRM" in
    [Yy]* )
        git commit -m "${VERSION}"
        git push -u origin "${BRANCH_NAME}"
        echo "Branch ${BRANCH_NAME} pushed to origin."
        ;;
    [Nn]* )
        echo "Changes staged but not committed or pushed. Exiting."
        exit 0
        ;;
    * )
        echo "Invalid input. Exiting."
        exit 1
        ;;
esac
