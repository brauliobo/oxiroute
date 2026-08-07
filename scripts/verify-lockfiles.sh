#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

for manifest in Cargo.toml fuzz/Cargo.toml benchmarks/loadgen/Cargo.toml; do
  cargo +1.97.1 metadata \
    --manifest-path "${repo_dir}/${manifest}" \
    --format-version 1 \
    --all-features \
    --no-deps \
    --locked \
    >/dev/null
done

for directory in ui remotion; do
  pnpm --dir "${repo_dir}/${directory}" install \
    --frozen-lockfile \
    --lockfile-only \
    --ignore-scripts \
    --prefer-offline
done

printf 'verified Cargo and pnpm lockfiles for the repository dependency roots\n'
