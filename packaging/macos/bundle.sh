#!/bin/sh
# Builds "target/K3 Up.app" from release binaries. Run from the repository root.
set -eu
cargo build --locked --release --features desktop
app="target/K3 Up.app/Contents"
rm -rf "target/K3 Up.app"
mkdir -p "$app/MacOS" "$app/Resources"
cp packaging/macos/Info.plist "$app/Info.plist"
cp packaging/macos/K3Up.icns "$app/Resources/K3Up.icns"
# macOS names login items after the executable, so the agent carries the product name.
cp target/release/k3up-desktop "$app/MacOS/K3 Up"
cp target/release/k3up-agent "$app/MacOS/K3 Up Agent"
cp target/release/k3up "$app/MacOS/k3up"
echo "Built target/K3 Up.app"
