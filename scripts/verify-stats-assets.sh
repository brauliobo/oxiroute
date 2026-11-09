#!/usr/bin/env bash
# Fails when the stats page assets embedded in the binary are not what the UI source builds.
#
# 1. The generated table (src/observability/stats_page/assets/table.rs) must match the files in
#    crates/oxiroute-server/assets/stats (no node needed).
# 2. `pnpm build:stats` is rebuilt from ui/stats.html (the single source of the shell) into ui/dist-stats and
#    compared byte for byte with the embedded files. Run it after `pnpm install --frozen-lockfile`.
#
# Refresh the embedded files with: scripts/generate-stats-assets.sh --from ui/dist-stats
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "${repo_dir}"

scripts/generate-stats-assets.sh --check

[[ -d ui/node_modules ]] || pnpm --dir ui install --frozen-lockfile --prefer-offline

rm -rf -- ui/dist-stats
pnpm --dir ui build:stats
scripts/generate-stats-assets.sh --from ui/dist-stats --check
