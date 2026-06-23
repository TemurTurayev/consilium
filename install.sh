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
echo "Downloading consilium ($target)…"
mkdir -p "$BINDIR"
curl -fsSL "$url" | tar -xz -C "$BINDIR"
chmod +x "$BINDIR/consilium"
echo ""
echo "✓ Installed consilium to $BINDIR/consilium"
case ":$PATH:" in
  *":$BINDIR:"*) : ;;
  *) echo "  Add it to your PATH:  export PATH=\"$BINDIR:\$PATH\"" ;;
esac
echo ""
echo "Next:  consilium init      (set up your council)"
