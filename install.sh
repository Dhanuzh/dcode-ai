#!/usr/bin/env bash
set -euo pipefail

VERSION="0.0.1"
REPO="Dhanuzh/dcode-ai"
BINARY="dcode-ai"
INSTALL_DIR="${DCODE_AI_INSTALL_DIR:-/usr/local/bin}"

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

tmpdir=""
cleanup() { [ -n "$tmpdir" ] && rm -rf "$tmpdir"; }
trap cleanup EXIT

main() {
    local platform target_url archive

    info "Installing ${BINARY} v${VERSION}"

    platform="$(detect_platform)"
    target_url="https://github.com/${REPO}/releases/download/v${VERSION}/${BINARY}-${platform}.tar.gz"

    info "Platform: ${platform}"
    info "Downloading from: ${target_url}"

    tmpdir="$(mktemp -d)"

    archive="${tmpdir}/${BINARY}.tar.gz"

    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$target_url" -o "$archive"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$archive" "$target_url"
    else
        error "Neither curl nor wget found. Install one and try again."
    fi

    tar xzf "$archive" -C "$tmpdir"

    if [ -w "$INSTALL_DIR" ]; then
        mv "${tmpdir}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    else
        info "Requires sudo to install to ${INSTALL_DIR}"
        sudo mv "${tmpdir}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    fi

    chmod +x "${INSTALL_DIR}/${BINARY}"

    info "Installed ${BINARY} v${VERSION} to ${INSTALL_DIR}/${BINARY}"
    info "Run 'dcode-ai --help' to get started"
}

main "$@"
