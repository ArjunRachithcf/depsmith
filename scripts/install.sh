#!/bin/sh
# Install the depsmith executable from a GitHub release, verified against the
# release's SHA256SUMS before anything is installed. Shell startup files are
# never edited; add the prefix to PATH yourself.
set -eu

usage() {
    cat <<'EOF'
Install depsmith from a GitHub release (checked against its SHA256SUMS).

  curl -fsSL https://github.com/ArjunRachithcf/depsmith/releases/latest/download/install.sh | sh
  curl -fsSL .../install.sh | sh -s -- --pre --prefix ~/bin

Options:
  --version TAG   release tag to install, such as v0.1.0 (default: latest)
  --pre           with no --version, take the newest release including
                  pre-releases (asks the GitHub API)
  --prefix DIR    directory for the executable (default: ~/.local/bin)
  --target TRIPLE override the detected platform
  --base-url URL  releases URL, for mirrors and tests
                  (default: https://github.com/ArjunRachithcf/depsmith/releases)
EOF
}

REPO="ArjunRachithcf/depsmith"
version=""
pre=0
prefix="${HOME}/.local/bin"
target=""
base="https://github.com/${REPO}/releases"
api="${DEPSMITH_INSTALL_API:-https://api.github.com/repos/${REPO}/releases}"

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
        -h | --help) usage; exit 0 ;;
        *) usage >&2; fail "unknown option $1" ;;
    esac
done

# HTTPS only, except a local test server. curl also refuses redirects to
# plain HTTP; wget cannot, so curl is preferred.
proto="=https"
case "$base" in http://127.0.0.1:* | http://localhost:*) proto="=https,http" ;; esac
if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL --proto "$proto" --proto-redir "$proto" -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q -O "$2" "$1"; }
else
    fail "needs curl or wget"
fi

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

if [ -z "$version" ] && [ "$pre" = 1 ]; then
    # Releases come newest first; the first tag includes pre-releases.
    version=$(fetch "${api}?per_page=1" - | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1) || true
    [ -n "$version" ] || fail "cannot find the newest release (GitHub API rate limit?); pass --version vX.Y.Z"
fi
if [ -n "$version" ]; then
    from="${base}/download/${version}"
else
    from="${base}/latest/download"
fi

asset="depsmith-${target}.tar.gz"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
trap 'exit 130' INT TERM
fetch "${from}/${asset}" "$work/$asset" || fail "cannot download ${asset} from ${from}"
fetch "${from}/SHA256SUMS" "$work/SHA256SUMS" || fail "cannot download SHA256SUMS from ${from}"

expected=$(awk -v name="$asset" '$2 == name || $2 == "*" name { print $1 }' "$work/SHA256SUMS")
[ -n "$expected" ] || fail "SHA256SUMS has no entry for ${asset}"
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

printf 'Installed depsmith %s (%s) to %s\n' "${version:-latest}" "$target" "$prefix/depsmith"
case ":${PATH}:" in
    *":${prefix}:"*) ;;
    *) printf 'Add %s to your PATH to run depsmith.\n' "$prefix" ;;
esac
