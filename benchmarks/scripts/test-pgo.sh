#!/usr/bin/env bash
# pgo.sh appends the same path-remapping flags to the instrumented, the profile-use and the plain build (a fake cargo
# records the RUSTFLAGS it is invoked with), so a profile trained by one matches the other and no builder path is embedded.
set -euo pipefail

pgo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/pgo.sh
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
root=$(mktemp -d)
trap 'rm -rf -- "${root}"' EXIT

mkdir -p "${root}/bin" "${root}/work" "${root}/cargo-home"
cat >"${root}/bin/cargo" <<'CARGO'
#!/usr/bin/env bash
printf '%s\n' "${RUSTFLAGS:-}" >>"${FAKE_CARGO_LOG}"
CARGO
cat >"${root}/llvm-profdata" <<'LLVM'
#!/usr/bin/env bash
echo 'Total functions: 10'
LLVM
chmod +x "${root}/bin/cargo" "${root}/llvm-profdata"

run() {
  : >"${root}/log"
  [[ $1 == train ]] || : >"${root}/work/oxiroute.profdata"
  FAKE_CARGO_LOG=${root}/log PATH="${root}/bin:${PATH}" PGO_WORK="${root}/work" CARGO_HOME="${root}/cargo-home" \
    LLVM_PROFDATA="${root}/llvm-profdata" PGO_LANES=' ' "${pgo}" "$@" >"${root}/out" 2>&1 || true
}

expect_flag() {
  local line=$1 flag=$2
  grep -qF -- "${flag}" <<<"${line}" || { printf 'RUSTFLAGS missing %s: %s\n' "${flag}" "${line}" >&2; exit 1; }
}

for step in train build plain; do
  run "${step}"
  # The first cargo call of the step is the main build (the training also builds the load tool afterwards).
  line=$(sed -n 1p "${root}/log")
  expect_flag "${line}" "--remap-path-prefix=${repo}=/usr/src/debug/oxiroute"
  expect_flag "${line}" "--remap-path-prefix=${root}/cargo-home=/usr/src/debug/oxiroute/cargo"
  expect_flag "${line}" "--remap-path-prefix=${root}/work=/usr/src/debug/oxiroute/build"
done
run train; expect_flag "$(sed -n 1p "${root}/log")" -Cprofile-generate
run build; expect_flag "$(sed -n 1p "${root}/log")" -Cprofile-use

printf 'pgo path remap tests passed\n'
