#!/usr/bin/env bash
# Stage the linux release into a relocatable tarball:
#   dist/rustshot-<version>-linux-x86_64.tar.gz
# containing the binary, a .desktop entry and an install hint.
set -euo pipefail
cd "$(dirname "$0")/../.."

case "$(uname -s)" in
  Linux) ;;
  *) echo "error: package.sh must run on Linux (uname=$(uname -s))" >&2; exit 1 ;;
esac

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
bin=target/release/rustshot
[ -x "$bin" ] || { echo "error: $bin missing (cargo build --release first)" >&2; exit 1; }

stage="dist/stage-linux"
rm -rf "$stage"
mkdir -p "$stage"
cp "$bin" "$stage/rustshot"
cp packaging/linux/rustshot.desktop "$stage/"
cp LICENSE "$stage/LICENSE"
cp THIRD_PARTY_NOTICES.md "$stage/"
cat > "$stage/INSTALL.txt" <<EOF
rustshot $version (linux x86_64, X11)

1. Move the binary onto your PATH:
       install -Dm755 rustshot ~/.local/bin/rustshot
2. Install the desktop entry (global hotkeys via your session autostart):
       install -Dm644 rustshot.desktop ~/.local/share/applications/rustshot.desktop
3. Start the daemon (registers the global hotkeys):
       rustshot daemon

Requires libX11 at runtime (present on every X11/Xorg desktop).
Wayland sessions are not supported yet; run under XWayland for capture only.
EOF

mkdir -p dist
tar -C "$stage" -czf "dist/rustshot-$version-linux-x86_64.tar.gz" .
echo "wrote dist/rustshot-$version-linux-x86_64.tar.gz"
