#!/usr/bin/env bash
set -euo pipefail

REPO="Dhanuzh/dcode-ai"
BINARY="dcode-ai"
INSTALL_DIR="${DCODE_AI_INSTALL_DIR:-}"

info()  { printf '\033[1;34m=>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
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

    # Release builds: Linux x86_64, macOS x86_64 and arm64.
    if [ "${arch}-${os}" = "aarch64-unknown-linux-gnu" ]; then
        error "No prebuilt binary for Linux arm64 yet; build from source: cargo build --release -p dcode-ai-cli"
    fi

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

verify_checksum() {
    # Args: <archive-file> <checksum-file>
    local archive checksum_file expected actual checksum_name
    archive="$1"
    checksum_file="$2"

    expected="$(awk '{print $1; exit}' "$checksum_file")"
    [ -n "$expected" ] || error "Checksum file is empty: $checksum_file"

    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$archive" | awk '{print $1}')"
        checksum_name="sha256sum"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$archive" | awk '{print $1}')"
        checksum_name="shasum"
    else
        warn "No sha256sum or shasum found; skipping checksum verification."
        return 0
    fi

    if [ "$actual" != "$expected" ]; then
        error "Checksum verification failed for $(basename "$archive")"
    fi

    info "Verified checksum with ${checksum_name}"
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

release_exists() {
    # Args: <tag>
    # Returns 0 if release exists, 1 otherwise.
    local tag api body
    tag="$1"
    api="https://api.github.com/repos/${REPO}/releases/tags/${tag}"
    body="$(http_get "$api" 2>/dev/null || true)"
    [ -n "$body" ] && ! printf '%s\n' "$body" | grep -q '"message":[[:space:]]*"Not Found"'
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
            error "Could not reach the GitHub API to find the latest release. Set DCODE_AI_VERSION=vX.Y.Z and retry."
        fi
    else
        if [[ "$requested" == v* ]]; then
            tag="$requested"
            version="${requested#v}"
        else
            version="$requested"
            tag="v${version}"
        fi

        # If the requested version does not have a published GitHub release yet,
        # fall back to latest published release instead of 404-ing on download.
        if ! release_exists "$tag"; then
            info "Requested release ${tag} not found; falling back to latest published release."
            tag="$(latest_release_tag)"
            if [ -z "$tag" ]; then
                error "Could not reach the GitHub API to find the latest release."
            else
                version="${tag#v}"
            fi
        fi
    fi

    if [ -z "${version:-}" ]; then
        version="${tag#v}"
    fi

    VERSION="$version"
    TAG="$tag"
}

# Where to install. A folder you own (~/.local/bin) means `dcode-ai update`
# never needs sudo; /usr/local/bin is used when it is already writable or
# when you choose it.
choose_install_dir() {
    [ -n "$INSTALL_DIR" ] && return
    if [ -w /usr/local/bin ]; then
        INSTALL_DIR=/usr/local/bin
        return
    fi
    local answer=""
    # Ask only when there is a terminal to answer on (curl | bash still has one).
    if { : </dev/tty; } 2>/dev/null; then
        printf '\033[1;34m=>\033[0m Install to ~/.local/bin (no sudo; updates never need sudo) [Y] or /usr/local/bin with sudo [s]? ' >/dev/tty
        read -r answer </dev/tty || answer=""
    fi
    case "$answer" in
        s|S|sudo) INSTALL_DIR=/usr/local/bin ;;
        *) INSTALL_DIR="$HOME/.local/bin" ;;
    esac
}

tmpdir=""
cleanup() { [ -n "$tmpdir" ] && rm -rf "$tmpdir"; }
trap cleanup EXIT

main() {
    local platform target_url checksum_url archive checksum_file

    resolve_version
    info "Installing ${BINARY} ${TAG}"

    platform="$(detect_platform)"
    target_url="https://github.com/${REPO}/releases/download/${TAG}/${BINARY}-${platform}.tar.gz"

    info "Platform: ${platform}"
    info "Downloading from: ${target_url}"

    tmpdir="$(mktemp -d)"

    archive="${tmpdir}/${BINARY}.tar.gz"
    http_download "$target_url" "$archive"
    checksum_url="${target_url}.sha256"
    checksum_file="${archive}.sha256"
    if http_download "$checksum_url" "$checksum_file" 2>/dev/null; then
        verify_checksum "$archive" "$checksum_file"
    else
        warn "Checksum file not found for ${TAG}; installing without archive verification."
    fi

    tar xzf "$archive" -C "$tmpdir"

    choose_install_dir
    case "$INSTALL_DIR" in
        "$HOME"/*) mkdir -p "$INSTALL_DIR" ;;
    esac
    if [ -w "$INSTALL_DIR" ]; then
        mv "${tmpdir}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    else
        info "Requires sudo to install to ${INSTALL_DIR}"
        sudo mv "${tmpdir}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    fi

    chmod +x "${INSTALL_DIR}/${BINARY}"

    info "Installed ${BINARY} ${TAG} to ${INSTALL_DIR}/${BINARY}"
    case ":$PATH:" in
        *":${INSTALL_DIR}:"*) ;;
        *) warn "${INSTALL_DIR} is not on your PATH. Add it: echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.bashrc  (or ~/.zshrc)" ;;
    esac
    local found
    found="$(command -v "$BINARY" 2>/dev/null || true)"
    if [ -n "$found" ] && [ "$found" != "${INSTALL_DIR}/${BINARY}" ]; then
        warn "Another ${BINARY} at ${found} comes first on your PATH; remove it (e.g. sudo rm ${found}) to use this one."
    fi
    check_provider_tools
    info "Run 'dcode-ai' to sign in and start; update later with 'dcode-ai update'"
}

# dcode-ai signs in through tools you install yourself; it never installs
# third-party binaries. Report what is missing for each provider.
check_provider_tools() {
    if command -v claude >/dev/null 2>&1; then
        info "Claude: found 'claude' (Claude account backend) — run 'claude auth login' if not signed in"
    else
        warn "Claude account backend needs Claude Code's 'claude' CLI: https://docs.claude.com/claude-code"
    fi
    if command -v ant >/dev/null 2>&1; then
        info "Claude API backend: found 'ant'"
    else
        info "Optional: Anthropic's 'ant' CLI enables the Console/API backend: https://github.com/anthropics/anthropic-cli/releases"
    fi
    info "GitHub Copilot: sign in with 'dcode-ai login copilot' (no extra tools needed)"
}

main "$@"
