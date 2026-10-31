#!/bin/sh
# Builds dist/basidekick-sim-<version>-linux-<arch>.tar.gz: the app, the
# terminal runner, a desktop launcher and install.sh.
set -e
cd "$(dirname "$0")/.."
VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
NAME="basidekick-sim-$VERSION-linux-$(uname -m)"

cargo build --release -p basidekick-sim -p basidekick-sim-cli

STAGE="dist/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp target/release/basidekick-sim target/release/basidekick-sim-cli "$STAGE/"
cp packaging/linux/com.basidekick.buildingsim.desktop packaging/linux/install.sh README.md LICENSE-MIT LICENSE-APACHE "$STAGE/"
cp assets/brand/icon-256.png "$STAGE/com.basidekick.buildingsim.png"
tar -C dist -czf "dist/$NAME.tar.gz" "$NAME"
echo "built dist/$NAME.tar.gz"
