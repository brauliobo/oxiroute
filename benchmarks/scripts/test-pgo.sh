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

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

key_fixture() {
  local dir=${root}/keyed
  rm -rf -- "${dir}"
  mkdir -p "${dir}/benchmarks/scripts" "${dir}/scripts/lib" "${dir}/crates/a/src" "${dir}/crates/a/tests" "${dir}/benchmarks/r2-data-path"
  cp -- "${pgo}" "${dir}/benchmarks/scripts/pgo.sh"
  cp -r -- "${repo}/scripts/lib/." "${dir}/scripts/lib/"
  printf 'fn a() {}\n' >"${dir}/crates/a/src/lib.rs"
  printf 'x\n' >"${dir}/crates/a/tests/t.rs"
  printf 'doc\n' >"${dir}/crates/a/README.md"
  printf 'x\n' >"${dir}/Cargo.lock"
  printf 'x\n' >"${dir}/Cargo.toml"
  printf 'x\n' >"${dir}/benchmarks/r2-data-path/run.py"
  git -C "${dir}" init -q
  git -C "${dir}" add -- .
  keyed=${dir}/benchmarks/scripts/pgo.sh
}

key_of() { "${keyed}" key "${1:-1.97.1}"; }

test_key_follows_the_inputs_of_the_profile() {
  local before
  key_fixture
  before=$(key_of)
  [[ ${before} == "$(key_of)" ]] || fail 'the key is not stable'
  [[ ${before} != "$(key_of 1.98.0)" ]] || fail 'the key ignores the toolchain'
  [[ ${before} != "$(PGO_TARGET=aarch64-unknown-linux-gnu key_of)" ]] || fail 'the key ignores the target'
  [[ ${before} != "$(PGO_CARGO_PROFILE=release-abort key_of)" ]] || fail 'the key ignores the cargo profile'
  [[ ${before} != "$(PGO_SCALE=1 key_of)" ]] || fail 'the key ignores the training scale'
  [[ ${before} != "$(PGO_LANES=h1_b1k key_of)" ]] || fail 'the key ignores the lane mix'
  printf 'documentation changed\n' >"${root}/keyed/crates/a/README.md"
  printf 'y\n' >"${root}/keyed/crates/a/tests/t.rs"
  git -C "${root}/keyed" add -- .
  [[ ${before} == "$(key_of)" ]] || fail 'a test or a document changes the key'
  printf 'fn a() { 1; }\n' >"${root}/keyed/crates/a/src/lib.rs"
  git -C "${root}/keyed" add -- .
  [[ ${before} != "$(key_of)" ]] || fail 'a source change keeps the key'
}

run_build() {
  FAKE_MISSING=$1 PATH="${root}/bin:${PATH}" PGO_WORK="${root}/work" CARGO_HOME="${root}/cargo-home" \
    LLVM_PROFDATA="${root}/llvm-profdata" "${pgo}" build >"${root}/out" 2>&1
}

test_stale_profile_is_a_hard_error() {
  local status=0
  : >"${root}/work/oxiroute.profdata"
  run_build 0 || fail 'a profile that matches every function is refused'
  run_build 1 || status=$?
  [[ ${status} == 3 ]] || fail "a profile that misses 1 of 10 functions exits ${status}, not 3"
  grep -q 'train it again' "${root}/out" || fail 'the refusal does not tell the job to train again'
}

test_given_profile_skips_the_training() {
  local build=${repo}/scripts/build-release-binaries.sh status=0
  rm -f -- "${root}/work/oxiroute.profdata" "${root}/log"
  PGO_PROFILE_GIVEN=1 STAGE=pgo PGO_WORK="${root}/work" PGO=1 BOLT=0 "${build}" 0.0.0 "${root}/out-given" >"${root}/out" 2>&1 || status=$?
  [[ ${status} != 0 ]] && grep -q 'there is no profile' "${root}/out" || fail 'a given profile that is missing is not refused'
  : >"${root}/work/oxiroute.profdata"
  FAKE_CARGO_LOG=${root}/log PATH="${root}/bin:${PATH}" PGO_PROFILE_GIVEN=1 STAGE=pgo PGO_WORK="${root}/work" PGO=1 BOLT=0 \
    CARGO_HOME="${root}/cargo-home" LLVM_PROFDATA="${root}/llvm-profdata" "${build}" 0.0.0 "${root}/out-given" >"${root}/out" 2>&1 || true
  ! grep -q 'stage-time training' "${root}/out" || fail 'a given profile is trained again'
  ! grep -q 'profile-generate' "${root}/out" || fail 'a given profile builds the instrumented binary'
  grep -q 'stage-time profile-use' "${root}/out" || fail 'a given profile is not used by the profile-use build'
}

test_release_script_prints_the_key_of_the_profile() {
  local key
  key=$(PGO_TOOLCHAIN=1.97.1 STAGE=key "${repo}/scripts/build-release-binaries.sh" 0.0.0 /dev/null)
  [[ ${key} == "$("${pgo}" key 1.97.1)" ]] || fail "the release script prints ${key}, not the key of pgo.sh"
  [[ ${key} != "$(FLAVOUR=abort PGO_TOOLCHAIN=1.97.1 STAGE=key "${repo}/scripts/build-release-binaries.sh" 0.0.0 /dev/null)" ]] ||
    fail 'the abort flavour shares the key of the unwind flavour'
}

test_stage_times_keep_the_status_and_are_logged() {
  local status=0
  # shellcheck source=/dev/null
  source "${repo}/scripts/lib/stage/time.sh"
  STAGE_TIMES=${root}/times timed passes true 2>/dev/null
  STAGE_TIMES=${root}/times timed fails false 2>/dev/null || status=$?
  [[ ${status} == 1 ]] || fail 'a failing stage loses its status'
  [[ $(cut -d' ' -f1,2 "${root}/times" | paste -sd,) == 'stage-time passes,stage-time fails' ]] || fail 'the stages are not logged'
}

cat >"${root}/bin/cargo" <<'CARGO'
#!/usr/bin/env bash
printf '%s\n' "${RUSTFLAGS:-}" >>"${FAKE_CARGO_LOG:-/dev/null}"
for _ in $(seq "${FAKE_MISSING:-0}"); do echo 'warning: no profile data available for function'; done
CARGO
test_key_follows_the_inputs_of_the_profile
test_stale_profile_is_a_hard_error
test_given_profile_skips_the_training
test_release_script_prints_the_key_of_the_profile
test_stage_times_keep_the_status_and_are_logged

printf 'pgo profile reuse tests passed\n'
