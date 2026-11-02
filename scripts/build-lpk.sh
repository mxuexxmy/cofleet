#!/bin/sh
# Invoked by lzc-cli; never recursively invokes project build.
set -eu
cd "$(dirname "$0")/.."
if ! command -v cargo >/dev/null 2>&1; then
  if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi
fi
cargo build --locked --release --target x86_64-unknown-linux-musl --bin cofleet-manager
mkdir -p .local/lpk-content dist
cp target/x86_64-unknown-linux-musl/release/cofleet-manager .local/lpk-content/cofleet-manager
cp deploy/lazycat/start-manager.sh .local/lpk-content/start-manager.sh
chmod 755 .local/lpk-content/cofleet-manager .local/lpk-content/start-manager.sh
