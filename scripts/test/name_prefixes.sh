#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
verify=${repo_dir}/scripts/verify/name_prefixes.sh
baseline_file=scripts/verify/name_prefixes_baseline.txt
guidance='Move the files into a subfolder named by the shared prefix'
root=$(mktemp -d)
work=${root}/work
trap 'rm -rf -- "${root}"' EXIT
source "${repo_dir}/scripts/lib/fixture_repo.sh"

new_repo
track src/process/management.rs
track src/process/rtmp_runtime.rs
track src/foo.rs
track src/foo/bar_one.rs
track src/foo/baz_two.rs
track ui/src/Panel.vue
track ui/src/Panel.test.ts
track src/lone_file.rs
track src/a/shared_one.rs
track src/b/shared_two.rs
track src/data_one.txt
track src/data_two.txt
track vendor/crate/shared_one.rs
track vendor/crate/shared_two.rs
track ui/src/api/generated/shared_one.ts
track ui/src/api/generated/shared_two.ts
expect_accepted 'distinct prefixes, module layout, other directories, other extensions and excluded paths'

track src/process_management.rs
track src/process_rtmp_runtime.rs
baseline '2 src/process'
expect_accepted 'a baselined group at its recorded count'

baseline '3 src/process'
expect_accepted 'a baselined group that shrank'

new_repo
track scripts/verify-one.sh
track scripts/verify-two.sh
expect_rejected 'a new group' 'scripts/verify: 2 files share a leading name prefix'

new_repo
track src/process_one.rs
track src/process_two.rs
track src/process_three.rs
baseline '2 src/process'
expect_rejected 'a baselined group that grew' 'src/process: grew from 2 to 3 files'

new_repo
track src/process_one.rs
track src/process_two.rs
baseline '2 src/process'
untrack src/process_two.rs
expect_rejected 'a stale baseline entry for a single remaining file' 'src/process: stale baseline entry'

new_repo
baseline '2 src/gone'
expect_rejected 'a baseline entry for a missing group' 'src/gone: stale baseline entry'

printf 'name_prefixes tests passed\n'
