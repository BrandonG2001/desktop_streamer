#!/bin/sh
# Installs desktop_streamer for the current user (or system-wide with PREFIX=/usr/local and sudo).
#
#   ./install.sh              install to ~/.local
#   ./install.sh --uinput     also install the udev rule for /dev/uinput (asks for sudo)
#   ./install.sh --uninstall  remove the files installed by this script
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
PREFIX=${PREFIX:-"$HOME/.local"}
BIN="$PREFIX/bin/desktop_streamer"
ICON="$PREFIX/share/icons/hicolor/256x256/apps/desktop_streamer.png"
DESKTOP="$PREFIX/share/applications/desktop_streamer.desktop"
RULE=/etc/udev/rules.d/60-desktop-streamer-uinput.rules

if [ "${1:-}" = "--uninstall" ]; then
    "$BIN" autostart off 2>/dev/null || true
    rm -f "$BIN" "$ICON" "$DESKTOP"
    echo "Removed desktop_streamer from $PREFIX (settings in ~/.config/virtual-desktop-streamer are kept)."
    exit 0
fi

install -Dm755 "$HERE/desktop_streamer" "$BIN"
install -Dm644 "$HERE/desktop_streamer.png" "$ICON"
mkdir -p "$(dirname "$DESKTOP")"
sed "s|@BIN@|$BIN|" "$HERE/desktop_streamer.desktop" > "$DESKTOP"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$(dirname "$DESKTOP")" || true
echo "Installed $BIN"

if [ "${1:-}" = "--uinput" ]; then
    sudo install -Dm644 "$HERE/60-desktop-streamer-uinput.rules" "$RULE"
    sudo udevadm control --reload-rules
    sudo udevadm trigger --name-match=uinput || true
    sudo usermod -aG input "$USER"
    echo "uinput access granted to the 'input' group; log out and back in to apply."
elif [ ! -w /dev/uinput ]; then
    echo "Note: /dev/uinput is not writable, so headset input will not work. Run: $0 --uinput"
fi

for tool in ffmpeg pactl parec pacat; do
    command -v "$tool" >/dev/null 2>&1 || echo "Note: '$tool' was not found; install it with your package manager."
done
