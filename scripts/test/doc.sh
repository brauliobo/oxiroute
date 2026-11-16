#!/usr/bin/env bash
# The documentation tests of the workspace. nextest does not run them, and they build the libraries, so CI runs them where
# the workspace is already built.
set -euo pipefail

cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
cargo test --workspace --doc --locked --jobs 4 "$@"
