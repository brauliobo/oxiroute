#!/usr/bin/env bash
# The documentation tests of the workspace. nextest does not run them, and they build the libraries, so CI runs them in a
# job of their own beside the archive job instead of delaying the test legs that wait for the archive.
set -euo pipefail

cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
cargo test --workspace --doc --locked --jobs 4 "$@"
