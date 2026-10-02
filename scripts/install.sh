#!/bin/sh
# Install the depsmith executable from a GitHub release, verified against the
# release's SHA256SUMS before anything is installed.
#
#   curl -fsSL https://github.com/ArjunRachithcf/depsmith/releases/latest/download/install.sh | sh
#   sh install.sh [--version vX.Y.Z] [--pre] [--prefix DIR] [--target TRIPLE]
#
# --version  release tag to install (default: the latest release; with --pre,
#            the latest release including pre-releases)
# --prefix   directory for the executable (default: ~/.local/bin)
# --target   override the detected target triple
# --base-url release download base (default: the GitHub releases of
#            ArjunRachithcf/depsmith); for tests and mirrors
#
# Shell startup files are never edited; add the prefix to PATH yourself.
set -eu

REPO="ArjunRachithcf/depsmith"
version=""
pre=0
prefix="${HOME}/.local/bin"
target=""
base="https://github.com/${REPO}/releases/download"

fail() {
    printf 'depsmith install: %s\n' "$1" >&2
    exit 1
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version) version="${2:?--version needs a tag}"; shift 2 ;;
        --pre) pre=1; shift ;;
        --prefix) prefix="${2:?--prefix needs a directory}"; shift 2 ;;
        --target) target="${2:?--target needs a triple}"; shift 2 ;;
        --base-url) base="${2:?--base-url needs a URL}"; shift 2 ;;
        -h | --help) sed -n '2,16p' "$0"; exit 0 ;;
        *) fail "unknown option $1 (see --help)" ;;
    esac
done

# HTTPS only (redirects included), except a local test server.
proto="=https"
https_only="--https-only"
case "$base" in http://127.0.0.1:* | http://localhost:*) proto="=https,http" https_only="" ;; esac

download() { # URL FILE
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --proto "$proto" --proto-redir "$proto" -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        # shellcheck disable=SC2086 # empty for local test servers
        wget -q $https_only -O "$2" "$1"
    else
        fail "needs curl or wget"
    fi
}

if [ -z "$target" ]; then
    case "$(uname -s)-$(uname -m)" in
        Linux-x86_64 | Linux-amd64) target="x86_64-unknown-linux-gnu" ;;
        Darwin-x86_64) target="x86_64-apple-darwin" ;;
        Darwin-arm64 | Darwin-aarch64) target="aarch64-apple-darwin" ;;
        *) target="$(uname -m)-$(uname -s)" ;;
    esac
fi
case "$target" in
    x86_64-unknown-linux-gnu | x86_64-apple-darwin | aarch64-apple-darwin) ;;
    *) fail "no prebuilt depsmith for ${target}; install it with 'cargo install depsmith' or 'pip install depsmith'" ;;
esac

if [ -z "$version" ]; then
    api="https://api.github.com/repos/${REPO}/releases"
    if [ "$pre" = 1 ]; then api="${api}?per_page=1"; else api="${api}/latest"; fi
    version=$(download "$api" - 2>/dev/null | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1) ||
        true
    [ -n "$version" ] || fail "cannot find the latest release; pass --version vX.Y.Z"
fi

asset="depsmith-${target}.tar.gz"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM
download "${base}/${version}/${asset}" "$work/$asset" || fail "cannot download ${asset} from ${version}"
download "${base}/${version}/SHA256SUMS" "$work/SHA256SUMS" || fail "cannot download SHA256SUMS from ${version}"

expected=$(awk -v name="$asset" '$2 == name || $2 == "*" name { print $1 }' "$work/SHA256SUMS")
[ -n "$expected" ] || fail "SHA256SUMS of ${version} has no entry for ${asset}"
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$work/$asset" | awk '{ print $1 }')
else
    actual=$(shasum -a 256 "$work/$asset" | awk '{ print $1 }')
fi
[ "$actual" = "$expected" ] || fail "checksum mismatch for ${asset} (expected ${expected}, got ${actual}); nothing installed"

mkdir "$work/unpacked"
tar -xzf "$work/$asset" -C "$work/unpacked"
[ -f "$work/unpacked/depsmith" ] || fail "${asset} has no depsmith executable"
mkdir -p "$prefix"
cp "$work/unpacked/depsmith" "$prefix/depsmith.new"
chmod 755 "$prefix/depsmith.new"
mv "$prefix/depsmith.new" "$prefix/depsmith"

printf 'Installed depsmith %s (%s) to %s\n' "$version" "$target" "$prefix/depsmith"
case ":${PATH}:" in
    *":${prefix}:"*) ;;
    *) printf 'Add %s to your PATH to run depsmith.\n' "$prefix" ;;
esac
