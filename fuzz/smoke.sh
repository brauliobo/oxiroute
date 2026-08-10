#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=/dev/null
source "${repo_dir}/fuzz/targets.sh"

optional_notice() {
    printf '%s\n' "$1"
    if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
        printf -- '- %s\n' "$1" >>"${GITHUB_STEP_SUMMARY}"
    fi
}

if ! command -v cargo-fuzz >/dev/null 2>&1; then
    optional_notice 'cargo-fuzz is unavailable; stable harness and corpus checks remain required, optional libFuzzer execution skipped.'
    exit 0
fi

if ! command -v rustup >/dev/null 2>&1; then
    optional_notice 'cargo-fuzz is available but rustup is unavailable; optional nightly libFuzzer execution skipped.'
    exit 0
fi

toolchains=$(rustup toolchain list 2>/dev/null || true)
if [[ "${toolchains}" != *nightly* ]]; then
    optional_notice 'cargo-fuzz is available but nightly Rust is unavailable; optional libFuzzer execution skipped.'
    exit 0
fi

export CARGO_BUILD_JOBS=4
fuzz_dir="${repo_dir}/fuzz"

if ! cargo +nightly fuzz list --fuzz-dir "${fuzz_dir}" >/dev/null; then
    printf '%s\n' 'cargo-fuzz and nightly Rust were detected, but cargo-fuzz could not list the fuzz targets; failing closed.' >&2
    exit 1
fi

for spec in "${FUZZ_TARGET_SPECS[@]}"; do
    IFS=: read -r target max_len <<<"${spec}"
    cargo +nightly fuzz run --fuzz-dir "${fuzz_dir}" "${target}" -- \
        -runs=32 \
        -seed=1 \
        -print_final_stats=1 \
        -max_len="$max_len" \
        -timeout=2 \
        -rss_limit_mb=1024 \
        -malloc_limit_mb=512
done
