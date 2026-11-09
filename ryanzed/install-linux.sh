#!/usr/bin/env bash
# Build RyanZed from this checkout and install it for the current Linux user.
#   ~/.local/ryanzed.app          the app (previous build kept as ryanzed.prev.app)
#   ~/.local/bin/ryanzed          launcher
#   ~/.local/share/applications/dev.ryanaross.RyanZed.desktop
# Builds whatever is checked out; ci.sh checks out the tagged SHA first.
set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
cd "$(dirname "$(readlink -f "$0")")/.."

script/bundle-linux --skip-sentry

tmp="$(mktemp -d)"
tar -xzf "target/release/zed-linux-$(uname -m).tar.gz" -C "$tmp"
rm -rf ~/.local/ryanzed.prev.app
if [[ -d ~/.local/ryanzed.app ]]; then mv ~/.local/ryanzed.app ~/.local/ryanzed.prev.app; fi
mv "$tmp/zed.app" ~/.local/ryanzed.app
rmdir "$tmp"

mkdir -p ~/.local/bin ~/.local/share/applications
cat > ~/.local/bin/ryanzed <<'LAUNCHER'
#!/usr/bin/env bash
# RyanZed. Event-based file watching (the SMB fix also used by zed-safe).
export ZED_FILE_WATCHER_MODE=native
exec "$HOME/.local/ryanzed.app/bin/zed" "$@"
LAUNCHER
chmod +x ~/.local/bin/ryanzed

# No x-scheme-handler/zed: official Zed keeps the zed:// links.
cat > ~/.local/share/applications/dev.ryanaross.RyanZed.desktop <<DESKTOP
[Desktop Entry]
Version=1.0
Type=Application
Name=RyanZed
GenericName=Text Editor
TryExec=$HOME/.local/bin/ryanzed
Exec=$HOME/.local/bin/ryanzed %U
Icon=$HOME/.local/ryanzed.app/share/icons/hicolor/512x512/apps/zed.png
StartupWMClass=dev.ryanaross.RyanZed
StartupNotify=true
Categories=Utility;TextEditor;Development;IDE;
MimeType=text/plain;application/x-zerosize;
Actions=NewWorkspace;

[Desktop Action NewWorkspace]
Exec=$HOME/.local/bin/ryanzed --new %U
Name=Open a new workspace
DESKTOP
update-desktop-database ~/.local/share/applications 2>/dev/null || true
echo "installed ~/.local/ryanzed.app"
