#!/usr/bin/env bash
# Fails when the stats page assets embedded in the binary are not what the UI source builds.
#
# 1. The generated table (src/observability/stats_page/assets/table.rs) must match the files in
#    crates/oxiroute-server/assets/stats (no node needed).
# 2. Once the UI source exists (ui/stats.html), `pnpm build:stats` is rebuilt into ui/dist-stats and
#    compared byte for byte with the embedded files. Run it after `pnpm install --frozen-lockfile`.
#
# Refresh the embedded files with: scripts/generate-stats-assets.sh --from ui/dist-stats
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "${repo_dir}"

scripts/generate-stats-assets.sh --check

if [[ ! -f ui/stats.html ]]; then
  printf 'verify-stats-assets: ui/stats.html does not exist yet, so there is no UI source to rebuild;\n'
  printf 'verify-stats-assets: the embedded files are the placeholder shell and were only checked against their table.\n'
  exit 0
fi

grep -q '"build:stats"' ui/package.json || {
  printf 'verify-stats-assets: ui/stats.html exists but ui/package.json has no build:stats script\n' >&2
  exit 1
}
[[ -d ui/node_modules ]] || pnpm --dir ui install --frozen-lockfile --prefer-offline

rm -rf -- ui/dist-stats
pnpm --dir ui build:stats
scripts/generate-stats-assets.sh --from ui/dist-stats --check
