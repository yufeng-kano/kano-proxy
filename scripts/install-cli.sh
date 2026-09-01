#!/bin/sh
# kano-proxy CLI installer (docs/cli.md § Distribution).
#
#   curl -fsSL https://raw.githubusercontent.com/yufeng-kano/kano-proxy/main/scripts/install-cli.sh | sh
#
# Detects OS/arch, downloads the latest GitHub Release asset, verifies it
# against SHA256SUMS, and installs to ~/.local/bin (override with --dir or
# KANO_PROXY_INSTALL_DIR). No sudo, no package manager.

set -eu

REPO="yufeng-kano/kano-proxy"
INSTALL_DIR="${KANO_PROXY_INSTALL_DIR:-$HOME/.local/bin}"

while [ $# -gt 0 ]; do
  case "$1" in
    --dir)
      INSTALL_DIR="$2"
      shift 2
      ;;
    *)
      echo "unknown option: $1" >&2
      exit 1
      ;;
  esac
done

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Darwin)
    case "$arch" in
      arm64 | aarch64) target="aarch64-apple-darwin" ;;
      x86_64) target="x86_64-apple-darwin" ;;
      *) echo "unsupported macOS arch: $arch" >&2; exit 1 ;;
    esac
    ;;
  Linux)
    case "$arch" in
      x86_64) target="x86_64-unknown-linux-musl" ;;
      arm64 | aarch64) target="aarch64-unknown-linux-musl" ;;
      *) echo "unsupported Linux arch: $arch" >&2; exit 1 ;;
    esac
    ;;
  *)
    echo "unsupported OS: $os (Windows: use Scoop — see the repo README)" >&2
    exit 1
    ;;
esac

tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" |
  sed -n 's/^ *"tag_name": *"\([^"]*\)".*/\1/p' | head -1)"
[ -n "$tag" ] || { echo "could not resolve the latest release" >&2; exit 1; }
version="${tag#v}"
asset="kano-proxy-$version-$target.tar.gz"
base="https://github.com/$REPO/releases/download/$tag"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "downloading $asset ($tag)…"
curl -fsSL -o "$tmp/$asset" "$base/$asset"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"

expected="$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")"
[ -n "$expected" ] || { echo "SHA256SUMS has no entry for $asset" >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$asset" | awk '{ print $1 }')"
else
  actual="$(shasum -a 256 "$tmp/$asset" | awk '{ print $1 }')"
fi
[ "$actual" = "$expected" ] || { echo "checksum mismatch for $asset — aborting" >&2; exit 1; }

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$INSTALL_DIR"
install -m 0755 "$tmp/kano-proxy" "$INSTALL_DIR/kano-proxy"

echo "✓ installed kano-proxy $version to $INSTALL_DIR/kano-proxy"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "note: $INSTALL_DIR is not on your PATH" ;;
esac
