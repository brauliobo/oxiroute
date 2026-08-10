#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=/dev/null
source "${repo_dir}/fuzz/targets.sh"

fuzz_dir="${repo_dir}/fuzz"
campaign_seconds=${FUZZ_CAMPAIGN_SECONDS:-300}
campaign_runs=${FUZZ_CAMPAIGN_RUNS:-1000000}
timeout_seconds=${FUZZ_TIMEOUT_SECONDS:-2}
rss_limit_mb=${FUZZ_RSS_LIMIT_MB:-1024}
malloc_limit_mb=${FUZZ_MALLOC_LIMIT_MB:-512}

if [[ -n "${FUZZ_CAMPAIGN_DIR:-}" ]]; then
    campaign_dir=${FUZZ_CAMPAIGN_DIR}
    if [[ "${campaign_dir}" != /* ]]; then
        printf '%s\n' 'FUZZ_CAMPAIGN_DIR must be an absolute path outside the repository.' >&2
        exit 2
    fi
else
    campaign_dir=$(mktemp -d "${TMPDIR:-/tmp}/oxiroute-fuzz-campaign.XXXXXX")
fi

case "${campaign_dir}" in
    "${repo_dir}"|"${repo_dir}"/*)
        printf '%s\n' "Campaign output must be outside the repository: ${campaign_dir}" >&2
        exit 2
        ;;
esac

if ! [[ "${campaign_seconds}" =~ ^[1-9][0-9]*$ ]]; then
    printf '%s\n' "FUZZ_CAMPAIGN_SECONDS must be a positive integer: ${campaign_seconds}" >&2
    exit 2
fi
if ! [[ "${campaign_runs}" =~ ^[1-9][0-9]*$ ]]; then
    printf '%s\n' "FUZZ_CAMPAIGN_RUNS must be a positive integer: ${campaign_runs}" >&2
    exit 2
fi
if ! [[ "${timeout_seconds}" =~ ^[1-9][0-9]*$ ]]; then
    printf '%s\n' "FUZZ_TIMEOUT_SECONDS must be a positive integer: ${timeout_seconds}" >&2
    exit 2
fi

mkdir -p "${campaign_dir}/corpus" "${campaign_dir}/artifacts" "${campaign_dir}/logs"

summary="${campaign_dir}/summary.txt"
tooling_report="${campaign_dir}/tooling.tsv"
results_report="${campaign_dir}/results.tsv"
target_list_log="${campaign_dir}/target-list.log"

printf 'campaign_dir=%s\nseconds_per_target=%s\nruns_per_target=%s\ntimeout_seconds=%s\n' \
    "${campaign_dir}" "${campaign_seconds}" "${campaign_runs}" "${timeout_seconds}" >"${summary}"
printf 'tool\tstatus\tversion\n' >"${tooling_report}"
printf 'target\tmax_len\tmax_total_time\truns\ttimeout\tresult\tduration_seconds\tcrash_count\tlog\tartifacts\n' \
    >"${results_report}"

record_tool() {
    local name=$1
    local status=$2
    local version=$3
    version=${version//$'\n'/ }
    version=${version//$'\t'/ }
    printf '%s\t%s\t%s\n' "${name}" "${status}" "${version}" >>"${tooling_report}"
}

report() {
    printf '%s\n' "$1"
    printf '%s\n' "$1" >>"${summary}"
}

unavailable_tools=()
broken_tools=()

mark_unavailable() {
    unavailable_tools+=("$1")
}

mark_broken() {
    broken_tools+=("$1")
}

cargo_fuzz_bin=$(command -v cargo-fuzz || true)
if [[ -z "${cargo_fuzz_bin}" ]]; then
    record_tool cargo-fuzz unavailable 'not found on PATH'
    mark_unavailable cargo-fuzz
else
    if ! cargo_fuzz_version=$("${cargo_fuzz_bin}" --version 2>&1); then
        record_tool cargo-fuzz broken "${cargo_fuzz_version}"
        mark_broken cargo-fuzz
    else
        record_tool cargo-fuzz available "${cargo_fuzz_version}"
    fi
fi

rustup_bin=$(command -v rustup || true)
if [[ -z "${rustup_bin}" ]]; then
    record_tool rustup unavailable 'not found on PATH'
    record_tool nightly unavailable 'rustup is not installed'
    mark_unavailable rustup
    mark_unavailable nightly
else
    if ! rustup_version=$(rustup --version 2>&1); then
        record_tool rustup broken "${rustup_version}"
        record_tool nightly unavailable 'rustup version check failed'
        mark_broken rustup
    else
        record_tool rustup available "${rustup_version}"
        if ! toolchains=$(rustup toolchain list 2>&1); then
            record_tool nightly broken 'rustup toolchain list failed'
            mark_broken nightly
        elif [[ "${toolchains}" != *nightly* ]]; then
            record_tool nightly unavailable 'not installed'
            mark_unavailable nightly
        elif ! nightly_rustc_version=$(rustup run nightly rustc --version 2>&1); then
            record_tool nightly broken "${nightly_rustc_version}"
            mark_broken nightly
        elif ! nightly_cargo_version=$(rustup run nightly cargo --version 2>&1); then
            record_tool nightly broken "${nightly_cargo_version}"
            mark_broken nightly
        else
            record_tool nightly available "${nightly_rustc_version}; ${nightly_cargo_version}"
        fi
    fi
fi

for llvm_tool in clang llvm-config ld.lld; do
    llvm_tool_path=$(command -v "${llvm_tool}" || true)
    if [[ -z "${llvm_tool_path}" ]]; then
        record_tool "${llvm_tool}" unavailable 'not found on PATH'
        mark_unavailable "${llvm_tool}"
    else
        if ! llvm_tool_version=$("${llvm_tool}" --version 2>&1); then
            record_tool "${llvm_tool}" broken "${llvm_tool_version}"
            mark_broken "${llvm_tool}"
        else
            record_tool "${llvm_tool}" available "${llvm_tool_version}"
        fi
    fi
done

if ((${#broken_tools[@]} > 0)); then
    report "campaign failed closed: detected tooling is broken (${broken_tools[*]}); no fuzz target was run."
    report "evidence: ${campaign_dir}"
    exit 1
fi
if ((${#unavailable_tools[@]} > 0)); then
    report "campaign unavailable: required tooling is unavailable (${unavailable_tools[*]}); no fuzz target was run."
    report "evidence: ${campaign_dir}"
    exit 2
fi

export CARGO_BUILD_JOBS=4
export CARGO_TARGET_DIR="${campaign_dir}/target"
export RUSTUP_TOOLCHAIN=nightly

if ! target_list=$("${cargo_fuzz_bin}" list --fuzz-dir "${fuzz_dir}" 2>&1); then
    printf '%s\n' "${target_list}" >"${target_list_log}"
    report 'campaign failed closed: detected cargo-fuzz/nightly tooling could not list fuzz targets.'
    report "evidence: ${campaign_dir}"
    exit 1
fi
printf '%s\n' "${target_list}" >"${target_list_log}"

for spec in "${FUZZ_TARGET_SPECS[@]}"; do
    IFS=: read -r target max_len <<<"${spec}"
    if [[ "${target_list}" != *"${target}"* ]]; then
        report "campaign failed closed: cargo-fuzz list did not include target ${target}."
        report "evidence: ${campaign_dir}"
        exit 1
    fi
done

failures=0
for spec in "${FUZZ_TARGET_SPECS[@]}"; do
    IFS=: read -r target max_len <<<"${spec}"
    run_corpus="${campaign_dir}/corpus/${target}"
    artifact_dir="${campaign_dir}/artifacts/${target}"
    log="${campaign_dir}/logs/${target}.log"
    mkdir -p "${run_corpus}" "${artifact_dir}"
    cp -a "${repo_dir}/fuzz/corpus/${target}/." "${run_corpus}/"

    printf 'target=%s\nmax_len=%s\nmax_total_time=%s\nruns=%s\ntimeout=%s\ncommand=' \
        "${target}" "${max_len}" "${campaign_seconds}" "${campaign_runs}" "${timeout_seconds}" >"${log}"
    printf '%q ' "${cargo_fuzz_bin}" run --fuzz-dir "${fuzz_dir}" "${target}" "${run_corpus}" -- \
        "-max_total_time=${campaign_seconds}" "-runs=${campaign_runs}" "-seed=1" \
        "-max_len=${max_len}" "-timeout=${timeout_seconds}" "-rss_limit_mb=${rss_limit_mb}" \
        "-malloc_limit_mb=${malloc_limit_mb}" -print_final_stats=1 \
        "-artifact_prefix=${artifact_dir}/" >>"${log}"
    printf '\n--- output ---\n' >>"${log}"

    SECONDS=0
    set +e
    "${cargo_fuzz_bin}" run --fuzz-dir "${fuzz_dir}" "${target}" "${run_corpus}" -- \
        "-max_total_time=${campaign_seconds}" \
        "-runs=${campaign_runs}" \
        -seed=1 \
        "-max_len=${max_len}" \
        "-timeout=${timeout_seconds}" \
        "-rss_limit_mb=${rss_limit_mb}" \
        "-malloc_limit_mb=${malloc_limit_mb}" \
        -print_final_stats=1 \
        "-artifact_prefix=${artifact_dir}/" >>"${log}" 2>&1
    status=$?
    set -e
    duration=${SECONDS}

    shopt -s nullglob
    artifacts=("${artifact_dir}"/*)
    shopt -u nullglob
    crash_count=0
    for artifact in "${artifacts[@]}"; do
        if [[ -f "${artifact}" ]]; then
            crash_count=$((crash_count + 1))
        fi
    done

    result=pass
    if ((crash_count > 0)); then
        result=crash
        failures=$((failures + 1))
    elif ((status != 0)); then
        result=failed
        failures=$((failures + 1))
    fi
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "${target}" "${max_len}" "${campaign_seconds}" "${campaign_runs}" "${timeout_seconds}" \
        "${result}" "${duration}" "${crash_count}" "${log}" "${artifact_dir}" >>"${results_report}"
    printf '%s: %s (%ss, crashes=%s)\n' "${target}" "${result}" "${duration}" "${crash_count}"
done

report "campaign evidence: ${campaign_dir}"
if ((failures > 0)); then
    report "campaign completed with ${failures} failed or crashing target(s); inspect per-target logs and artifacts before treating it as evidence."
    exit 1
fi
report 'campaign completed: every target reached its bounded campaign limit without a recorded crash.'
