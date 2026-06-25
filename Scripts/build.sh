#!/usr/bin/env bash
# ============================================================================
#  DAUx Plugin Platform - build script (Linux / macOS, x64 & arm64)
#
#  Builds: Core + DAUxHost + DAUxScan + C++ example (CMake), the Rust wrapper
#  + example (cargo), and the .NET wrapper + Avalonia example (NativeAOT),
#  then assembles the .NET bundle into <root>/build/plugins.
#
#  Usage:  ./build.sh [all|core|rust|dotnet]    (default: all)
#
#  Requirements: cmake + a C/C++ toolchain (clang/gcc); cargo; .NET 8 SDK.
#  NativeAOT on Linux additionally needs clang + the objwriter deps; on macOS
#  the Xcode command-line tools.
# ============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DAUX_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ROOT_DIR="$(cd "$DAUX_DIR" && pwd)"
BUILD_DIR="$ROOT_DIR/build.$(uname)"
PLUGINS_DIR="$BUILD_DIR"
CONFIG="Release"
WHAT="${1:-all}"

# --- platform detection -----------------------------------------------------
OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
    Linux)  EXT=".so";    OSID="linux" ;;
    Darwin) EXT=".dylib"; OSID="osx" ;;
    *) echo "Unsupported OS: $OS (use build.cmd on Windows)"; exit 2 ;;
esac
case "$ARCH" in
    x86_64|amd64) RIDARCH="x64" ;;
    arm64|aarch64) RIDARCH="arm64" ;;
    *) echo "Unsupported arch: $ARCH"; exit 2 ;;
esac
RID="${OSID}-${RIDARCH}"

mkdir -p "$PLUGINS_DIR"

build_core() {
    echo "=== [1/3] Core + Host + C++ example (CMake) ==="
    cmake -S "$DAUX_DIR" -B "$BUILD_DIR" -DCMAKE_BUILD_TYPE="$CONFIG"
    cmake --build "$BUILD_DIR" -j
}

build_rust() {
    echo "=== [2/3] Rust wrapper + example (cargo) ==="
    ( cd "$DAUX_DIR/Examples/GainRustGpui" && cargo build --release )
    # cdylib output is lib<name>.<ext> on Linux/macOS.
    local out="$DAUX_DIR/Examples/GainRustGpui/target/release/libgain_rust_gpui${EXT}"
    cp -f "$out" "$PLUGINS_DIR/daux_gain_rust.dauxplug"
    echo "    -> $PLUGINS_DIR/daux_gain_rust.dauxplug"
}

build_dotnet() {
    echo "=== [3/3] .NET wrapper + Avalonia example (NativeAOT bundle) ==="
    local proj="$DAUX_DIR/Examples/GainDotnetAvalonia/Plugin/gain-dotnet-avalonia.csproj"
    dotnet publish "$proj" -r "$RID" -c "$CONFIG"

    local pub="$DAUX_DIR/Examples/GainDotnetAvalonia/Plugin/bin/$CONFIG/net8.0/$RID/publish"
    local bundle="$PLUGINS_DIR/daux_gain_dotnet.dauxplug"

    rm -rf "$bundle"
    mkdir -p "$bundle/Exec" "$bundle/Library" "$bundle/Resources"

    # Entry module (NativeAOT shared lib; with or without 'lib' prefix).
    local entry=""
    for cand in "$pub/gain-dotnet-avalonia${EXT}" "$pub/libgain-dotnet-avalonia${EXT}"; do
        [ -e "$cand" ] && { entry="$cand"; break; }
    done
    if [ -z "$entry" ]; then
        echo "error: could not find the published entry module in $pub" >&2
        exit 1
    fi
    cp -f "$entry" "$bundle/Exec/daux_gain_dotnet${EXT}"

    # Every other shared library (Skia, HarfBuzz, ...) -> Library/.
    shopt -s nullglob
    for f in "$pub"/*.so "$pub"/*.dylib; do
        [ "$f" = "$entry" ] && continue
        cp -f "$f" "$bundle/Library/$(basename "$f")"
    done
    shopt -u nullglob

    cp -f "$DAUX_DIR/Examples/GainDotnetAvalonia/manifest.xml" "$bundle/manifest.xml"
    echo "    -> $bundle (bundle)"
}

run_scan() {
    echo "=== Verifying with DAUxScan ==="
    local scan="$BUILD_DIR/bin/DAUxScan"
    if [ -x "$scan" ]; then
        "$scan" "$PLUGINS_DIR" || true
    else
        echo "(DAUxScan not built; run with 'core' or 'all' first.)"
    fi
    echo
    echo "Build complete. Plugins in: $PLUGINS_DIR"
}

case "$WHAT" in
    all)    build_core; build_rust; build_dotnet; run_scan ;;
    core)   build_core; run_scan ;;
    rust)   build_rust; run_scan ;;
    dotnet) build_dotnet; run_scan ;;
    *) echo "Unknown component \"$WHAT\". Use: all | core | rust | dotnet"; exit 2 ;;
esac
