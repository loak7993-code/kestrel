#!/usr/bin/env bash
# kestrel installer — downloads the latest release binary for this platform
# curl -sSL https://raw.githubusercontent.com/loak7993-code/kestrel/main/install.sh | bash
set -euo pipefail
REPO=loak7993-code/kestrel
OS=$(uname -s | tr "[:upper:]" "[:lower:]")
ARCH=$(uname -m)
case "$OS-$ARCH" in
  linux-x86_64) NAME=kestrel-x86_64-linux ;;
  linux-aarch64) NAME=kestrel-aarch64-linux ;;
  darwin-arm64) NAME=kestrel-aarch64-macos ;;
  darwin-x86_64) NAME=kestrel-x86_64-macos ;;
  *) echo "unsupported platform: $OS-$ARCH (build from source: cargo install --path .)"; exit 1 ;;
esac
URL=$(curl -sL "https://api.github.com/repos/$REPO/releases/latest" | python3 -c "import json,sys; d=json.load(sys.stdin); print([a[\"browser_download_url\"] for a in d[\"assets\"] if a[\"name\"].startswith(\"$NAME\")][0])")
echo "downloading $URL"
curl -sL "$URL" -o /tmp/kestrel.tar.gz
tar xzf /tmp/kestrel.tar.gz -C /tmp
mkdir -p ~/.local/bin
mv /tmp/ksl ~/.local/bin/ksl
chmod +x ~/.local/bin/ksl
echo "installed: $(~/.local/bin/ksl --version) → ~/.local/bin/ksl (add ~/.local/bin to PATH)"
