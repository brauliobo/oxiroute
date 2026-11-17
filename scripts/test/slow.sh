#!/usr/bin/env bash
# The tests that wait out a real default duration (see the slow entries of scripts/ci/tiers.txt), on the default plane
# chain or the plane named by OXIROUTE_TEST_HTTP_DATA_PLANE. Run nightly and on demand, not on every pull request.
set -euo pipefail

# shellcheck source=../lib/nextest.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/../lib/nextest.sh"

OXIROUTE_TEST_BUDGET=none run_tiers slow "$@"
