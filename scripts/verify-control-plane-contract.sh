#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "${repo_dir}"

nice -n 10 cargo +1.97.1 test -p oxiroute --lib rtmp_api::contract --locked --jobs 4
pnpm --dir ui verify:control-plane
