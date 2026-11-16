#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
verify=${repo_dir}/scripts/verify/line_limit.sh
baseline_file=scripts/verify/line_limit_baseline.txt
guidance='Split each oversized file into modules or subfolders'
root=$(mktemp -d)
work=${root}/work
trap 'rm -rf -- "${root}"' EXIT
source "${repo_dir}/scripts/lib/fixture_repo.sh"

new_repo
track src/at_limit.rs 1000
track src/notes.txt 5000
track vendor/crate/big.rs 5000
track fuzz/corpus/big.py 5000
track ui/src/api/generated/types.ts 5000
track crates/oxiroute-server/src/observability/stats_page/assets/table.rs 5000
expect_accepted 'files within the limit and excluded paths'

track src/old.rs 1200
baseline '1200 src/old.rs'
expect_accepted 'a baselined oversized file at its recorded count'

track src/old.rs 1100
expect_accepted 'a baselined file that shrank'

new_repo
track src/new.rs 1001
expect_rejected 'a new offender' 'src/new.rs: 1001 lines exceeds the 1000 line limit'

new_repo
track src/old.rs 1300
baseline '1200 src/old.rs'
expect_rejected 'a baselined file that grew' 'src/old.rs: grew from 1200 to 1300 lines'

new_repo
track src/old.rs 1000
baseline '1200 src/old.rs'
expect_rejected 'a stale baseline entry for a file within the limit' 'src/old.rs: stale baseline entry'

new_repo
baseline '1200 src/gone.rs'
expect_rejected 'a baseline entry for a missing file' 'src/gone.rs: stale baseline entry'

printf 'line_limit tests passed\n'
