#!/bin/sh
# Builds "dist/BASidekick Building Sim.app" and a zip of it for sharing.
#   scripts/build-mac.sh              release build for this Mac
#   scripts/build-mac.sh --universal  one app for Apple silicon and Intel
#   scripts/build-mac.sh --debug      bundle the debug build (quick, for trying the UI)
# The terminal runner, basidekick-sim-cli, goes inside the app next to it.
set -e
cd "$(dirname "$0")/.."
NAME="BASidekick Building Sim"
VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
APP="dist/$NAME.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

case "$1" in
  --debug)
    cargo build -p basidekick-sim -p basidekick-sim-cli
    cp target/debug/basidekick-sim target/debug/basidekick-sim-cli "$APP/Contents/MacOS/"
    ARCH=$(uname -m)
    ;;
  --universal)
    rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
    for t in aarch64-apple-darwin x86_64-apple-darwin; do
      cargo build --release --target $t -p basidekick-sim -p basidekick-sim-cli
    done
    for b in basidekick-sim basidekick-sim-cli; do
      lipo -create -output "$APP/Contents/MacOS/$b" \
        "target/aarch64-apple-darwin/release/$b" "target/x86_64-apple-darwin/release/$b"
    done
    ARCH=universal
    ;;
  *)
    cargo build --release -p basidekick-sim -p basidekick-sim-cli
    cp target/release/basidekick-sim target/release/basidekick-sim-cli "$APP/Contents/MacOS/"
    ARCH=$(uname -m)
    ;;
esac

# app icon from the brand's 1024 px icon
ICONSET="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z $size $size assets/brand/icon-1024.png --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  sips -z $((size * 2)) $((size * 2)) assets/brand/icon-1024.png --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>$NAME</string>
  <key>CFBundleDisplayName</key><string>$NAME</string>
  <key>CFBundleIdentifier</key><string>com.basidekick.buildingsim</string>
  <key>CFBundleExecutable</key><string>basidekick-sim</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSLocalNetworkUsageDescription</key><string>The simulated building answers BACnet/IP on your local network.</string>
</dict></plist>
PLIST
# ad-hoc signature: runs on this Mac; others right-click > Open the first time
codesign --force --deep --sign - "$APP"

ZIP="dist/basidekick-sim-$VERSION-macos-$ARCH.zip"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"
echo "built $APP and $ZIP"
