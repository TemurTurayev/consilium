#!/bin/sh
# Consilium installer — downloads the latest prebuilt binary for your platform.
# Usage: curl -fsSL https://raw.githubusercontent.com/TemurTurayev/consilium/main/install.sh | sh
set -e
REPO="TemurTurayev/consilium"
BINDIR="${BINDIR:-$HOME/.local/bin}"
os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Darwin)
    case "$arch" in
      arm64|aarch64) target="aarch64-apple-darwin" ;;
      x86_64) target="x86_64-apple-darwin" ;;
      *) echo "consilium: unsupported macOS arch: $arch" >&2; exit 1 ;;
    esac ;;
  Linux)
    case "$arch" in
      x86_64) target="x86_64-unknown-linux-gnu" ;;
      *) echo "consilium: unsupported Linux arch: $arch (build from source with cargo)" >&2; exit 1 ;;
    esac ;;
  *) echo "consilium: unsupported OS: $os" >&2; exit 1 ;;
esac
url="https://github.com/$REPO/releases/latest/download/consilium-$target.tar.gz"
tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT
echo "Downloading consilium ($target)…"
curl -fsSL "$url" -o "$tmpdir/consilium-$target.tar.gz"
if curl -fsSL "$url.sha256" -o "$tmpdir/consilium-$target.tar.gz.sha256" 2>/dev/null; then
  echo "Verifying checksum…"
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$tmpdir" && sha256sum -c "consilium-$target.tar.gz.sha256" >/dev/null 2>&1) \
      || { echo "consilium: checksum verification FAILED — aborting install" >&2; exit 1; }
  else
    (cd "$tmpdir" && shasum -a 256 -c "consilium-$target.tar.gz.sha256" >/dev/null 2>&1) \
      || { echo "consilium: checksum verification FAILED — aborting install" >&2; exit 1; }
  fi
else
  echo "  (no checksum published for this release — skipping verification)"
fi
mkdir -p "$BINDIR"
tar -xz -C "$BINDIR" -f "$tmpdir/consilium-$target.tar.gz"
chmod +x "$BINDIR/consilium"
echo ""
echo "✓ Installed consilium to $BINDIR/consilium"
case ":$PATH:" in
  *":$BINDIR:"*) : ;;
  *) echo "  Add it to your PATH:  export PATH=\"$BINDIR:\$PATH\"" ;;
esac
echo ""
echo "Next:  consilium init      (set up your council)"
