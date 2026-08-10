#!/usr/bin/env bash

fuzz_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
specs=$(python3 "${fuzz_dir}/manifest.py" "${fuzz_dir}/targets.json") || return 1
mapfile -t FUZZ_TARGET_SPECS <<<"${specs}"
unset fuzz_dir specs
