#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REV="8a8954c7234f2261d13b72568ff09e4a5136d39f"
SHA256="45811227079b2dc10ea3defac62bf5d845fd6b1a9ffd532b56536526c6cf62bc"
SOURCE_DIR="$ROOT/vendor/zed-$REV"
LINK_DIR="$ROOT/vendor/zed"
ARCHIVE="${ZED_GPUI_ARCHIVE:-$ROOT/vendor/zed-$REV.tar.gz}"
URL="https://github.com/wingleeio/zed/archive/$REV.tar.gz"

mkdir -p "$ROOT/vendor"

if [ -e "$LINK_DIR" ] || [ -L "$LINK_DIR" ]; then
    if [ ! -d "$LINK_DIR" ]; then
        echo "GPUI vendor link exists but does not resolve: $LINK_DIR" >&2
        exit 1
    fi
    exit 0
fi

if [ ! -d "$SOURCE_DIR" ]; then
    if [ ! -f "$ARCHIVE" ]; then
        download="$ARCHIVE.download"
        if [ -e "$download" ]; then
            echo "refusing to replace partial download: $download" >&2
            exit 1
        fi
        curl --fail --location --retry 5 --retry-delay 2 --output "$download" "$URL"
        archive_to_check="$download"
    else
        archive_to_check="$ARCHIVE"
    fi

    if command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$archive_to_check" | cut -d' ' -f1)"
    elif command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$archive_to_check" | cut -d' ' -f1)"
    else
        echo "shasum or sha256sum is required" >&2
        exit 1
    fi

    if [ "$actual" != "$SHA256" ]; then
        echo "GPUI fork archive checksum mismatch" >&2
        echo "expected: $SHA256" >&2
        echo "actual:   $actual" >&2
        exit 1
    fi

    if [ ! -f "$ARCHIVE" ]; then
        mv "$download" "$ARCHIVE"
    fi
    tar -xzf "$ARCHIVE" -C "$ROOT/vendor"
fi

ln -s "zed-$REV" "$LINK_DIR"
