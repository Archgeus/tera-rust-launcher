#!/usr/bin/env bash
# install-linux-deps.sh — Install build dependencies for the native Linux TERA launcher
#
# Run this once on your Linux machine before building with cargo.
# Supports Debian/Ubuntu (apt) and Fedora/RHEL (dnf).

set -euo pipefail

info()  { echo "[INFO]  $*"; }
error() { echo "[ERROR] $*" >&2; exit 1; }

if command -v apt-get &>/dev/null; then
    info "Detected Debian/Ubuntu — using apt-get"
    sudo apt-get update
    sudo apt-get install -y \
        build-essential \
        pkg-config \
        libssl-dev \
        libgtk-3-dev \
        libayatana-appindicator3-dev \
        librsvg2-dev \
        libsoup2.4-dev \
        curl \
        wget \
        file \
        wine \
        wine64 \
        winetricks \
        gcc-mingw-w64-x86-64

    # webkit2gtk: Ubuntu 24.04+ ships 4.1 only; older releases ship 4.0
    if apt-cache show libwebkit2gtk-4.1-dev &>/dev/null; then
        info "Installing libwebkit2gtk-4.1-dev (Ubuntu 24.04+ / Debian 12+)"
        sudo apt-get install -y libwebkit2gtk-4.1-dev
        # Tauri v1 looks for webkit2gtk-4.0 via pkg-config — create an alias
        PKG40=/usr/lib/x86_64-linux-gnu/pkgconfig/webkit2gtk-4.0.pc
        PKG41=/usr/lib/x86_64-linux-gnu/pkgconfig/webkit2gtk-4.1.pc
        if [ ! -f "$PKG40" ] && [ -f "$PKG41" ]; then
            info "Creating pkg-config alias: webkit2gtk-4.0 → webkit2gtk-4.1"
            sudo ln -sf "$PKG41" "$PKG40"
            # Same for the JSCore companion
            JSCORE40=/usr/lib/x86_64-linux-gnu/pkgconfig/javascriptcoregtk-4.0.pc
            JSCORE41=/usr/lib/x86_64-linux-gnu/pkgconfig/javascriptcoregtk-4.1.pc
            if [ ! -f "$JSCORE40" ] && [ -f "$JSCORE41" ]; then
                sudo ln -sf "$JSCORE41" "$JSCORE40"
            fi
            # Also symlink the .so files so the linker finds -lwebkit2gtk-4.0
            LIB=/usr/lib/x86_64-linux-gnu
            [ ! -f "$LIB/libwebkit2gtk-4.0.so" ]       && sudo ln -sf "$LIB/libwebkit2gtk-4.1.so"       "$LIB/libwebkit2gtk-4.0.so"
            [ ! -f "$LIB/libjavascriptcoregtk-4.0.so" ] && sudo ln -sf "$LIB/libjavascriptcoregtk-4.1.so" "$LIB/libjavascriptcoregtk-4.0.so"
        fi
    elif apt-cache show libwebkit2gtk-4.0-dev &>/dev/null; then
        info "Installing libwebkit2gtk-4.0-dev (Ubuntu 22.04 / Debian 11)"
        sudo apt-get install -y libwebkit2gtk-4.0-dev
    else
        error "Could not find libwebkit2gtk-4.0-dev or libwebkit2gtk-4.1-dev in apt cache"
    fi

elif command -v dnf &>/dev/null; then
    info "Detected Fedora/RHEL — using dnf"
    sudo dnf install -y \
        gcc \
        openssl-devel \
        webkit2gtk4.0-devel \
        gtk3-devel \
        libappindicator-gtk3-devel \
        librsvg2-devel \
        curl \
        wget \
        file \
        wine \
        wine64 \
        winetricks \
        mingw64-gcc

else
    error "Unsupported package manager. Install the following manually:
  - libwebkit2gtk-4.0-dev (or webkit2gtk4.0-devel)
  - libssl-dev, pkg-config, libgtk-3-dev, librsvg2-dev
  - wine, winetricks
  - mingw-w64 (for cross-compiling launcher-bridge.exe)"
fi

# Install the Windows cross-compilation target for Rust
info "Adding x86_64-pc-windows-gnu Rust target …"
rustup target add x86_64-pc-windows-gnu || true

info ""
info "All dependencies installed."
info ""