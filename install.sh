#!/usr/bin/env bash
set -euo pipefail

REPO="Dhanuzh/dcode-ai"
BINARY="dcode-ai"
INSTALL_DIR="${DCODE_AI_INSTALL_DIR:-/usr/local/bin}"
# Fallback when API lookup fails and no explicit version is provided.
FALLBACK_VERSION="0.0.5"

info()  { printf '\033[1;34m=>\033[0m %s\n' "$*"; }
error() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

detect_platform() {
    local os arch

    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)  os="unknown-linux-gnu" ;;
        Darwin) os="apple-darwin" ;;
        *)      error "Unsupported OS: $os" ;;
    esac

    case "$arch" in
        x86_64|amd64)   arch="x86_64" ;;
        aarch64|arm64)  arch="aarch64" ;;
        *)              error "Unsupported architecture: $arch" ;;
    esac

    echo "${arch}-${os}"
}

http_get() {
    # Print response body to stdout.
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO- "$1"
    else
        error "Neither curl nor wget found. Install one and try again."
    fi
}

http_download() {
    # Args: <url> <dest-file>
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$2" "$1"
    else
        error "Neither curl nor wget found. Install one and try again."
    fi
}

latest_release_tag() {
    # Returns tag like "v0.1.2" on stdout.
    # No jq dependency; parse tag_name with sed.
    local api url body
    api="https://api.github.com/repos/${REPO}/releases/latest"
    url="${api}"
    body="$(http_get "$url" 2>/dev/null || true)"
    printf '%s\n' "$body" | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1
}

resolve_version() {
    # Input env:
    #   DCODE_AI_VERSION: "latest" (default) or explicit like "0.2.0" / "v0.2.0"
    # Output:
    #   VERSION (without leading "v"), TAG (with leading "v")
    local requested tag version
    requested="${DCODE_AI_VERSION:-latest}"

    if [ "$requested" = "latest" ] || [ -z "$requested" ]; then
        tag="$(latest_release_tag)"
        if [ -z "$tag" ]; then
            info "Could not resolve latest release from GitHub API; falling back to v${FALLBACK_VERSION}"
            version="${FALLBACK_VERSION}"
            tag="v${version}"
        fi
    else
        if [[ "$requested" == v* ]]; then
            tag="$requested"
            version="${requested#v}"
        else
            version="$requested"
            tag="v${version}"
        fi
    fi

    if [ -z "${version:-}" ]; then
        version="${tag#v}"
    fi

    VERSION="$version"
    TAG="$tag"
}

tmpdir=""
cleanup() { [ -n "$tmpdir" ] && rm -rf "$tmpdir"; }
trap cleanup EXIT

main() {
    local platform target_url archive

    resolve_version
    info "Installing ${BINARY} ${TAG}"

    platform="$(detect_platform)"
    target_url="https://github.com/${REPO}/releases/download/${TAG}/${BINARY}-${platform}.tar.gz"

    info "Platform: ${platform}"
    info "Downloading from: ${target_url}"

    tmpdir="$(mktemp -d)"

    archive="${tmpdir}/${BINARY}.tar.gz"
    http_download "$target_url" "$archive"

    tar xzf "$archive" -C "$tmpdir"

    if [ -w "$INSTALL_DIR" ]; then
        mv "${tmpdir}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    else
        info "Requires sudo to install to ${INSTALL_DIR}"
        sudo mv "${tmpdir}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    fi

    chmod +x "${INSTALL_DIR}/${BINARY}"

    info "Installed ${BINARY} ${TAG} to ${INSTALL_DIR}/${BINARY}"
    info "Run 'dcode-ai --help' to get started"
}

main "$@"
