#!/usr/bin/env bash
set -euo pipefail

if ! command -v cargo-fuzz >/dev/null 2>&1; then
    printf '%s\n' 'cargo-fuzz is unavailable; skipping optional fuzz execution.'
    exit 0
fi

if ! command -v rustup >/dev/null 2>&1; then
    printf '%s\n' 'rustup is unavailable; skipping optional cargo-fuzz execution.'
    exit 0
fi

toolchains=$(rustup toolchain list 2>/dev/null || true)
case "$toolchains" in
    *nightly*) ;;
    *)
        printf '%s\n' 'nightly Rust is unavailable; skipping optional cargo-fuzz execution.'
        exit 0
        ;;
esac

export CARGO_BUILD_JOBS=4

for target in config_source native_source forward_target overread_io rtmp_handshake rtmp_chunk rtmp_amf; do
    case "$target" in
        config_source|native_source) max_len=131072 ;;
        forward_target|overread_io) max_len=16384 ;;
        rtmp_handshake) max_len=131072 ;;
        rtmp_chunk) max_len=262144 ;;
        rtmp_amf) max_len=32768 ;;
    esac
    cargo +nightly fuzz run "$target" -- \
        -runs=32 \
        -max_len="$max_len" \
        -timeout=2 \
        -rss_limit_mb=256 \
        -malloc_limit_mb=128
done
