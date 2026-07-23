#!/usr/bin/env bash

set -euo pipefail
IFS=$'\n\t'

BENCHMARK_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
REPOSITORY_ROOT=$(cd -- "$BENCHMARK_ROOT/.." && pwd -P)
GENERATED_ROOT="$BENCHMARK_ROOT/generated"

declare -a BENCH_PIDS=()
declare -a BENCH_START_TIMES=()
declare -a BENCH_PROCESS_NAMES=()

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_positive_integer() {
  local name=$1
  local value=$2
  [[ $value =~ ^[1-9][0-9]*$ ]] || die "$name must be a positive integer"
}

process_fields() {
  local pid=$1
  local stat rest
  local -a parts
  [[ -r /proc/$pid/stat ]] || return 1
  IFS= read -r stat < "/proc/$pid/stat" || return 1
  rest=${stat##*) }
  IFS=' ' read -r -a parts <<< "$rest"
  [[ ${#parts[@]} -ge 20 ]] || return 1
  printf '%s %s\n' "${parts[0]}" "${parts[19]}"
}

start_process() {
  local name=$1
  local stdout_path=$2
  local stderr_path=$3
  local pid fields state start_time
  shift 3

  "$@" >"$stdout_path" 2>"$stderr_path" &
  pid=$!
  for _ in {1..50}; do
    if fields=$(process_fields "$pid"); then
      read -r state start_time <<< "$fields"
      BENCH_PIDS+=("$pid")
      BENCH_START_TIMES+=("$start_time")
      BENCH_PROCESS_NAMES+=("$name")
      return 0
    fi
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.02
  done
  wait "$pid" || true
  die "$name exited before its process identity could be recorded"
}

stop_process_index() {
  local index=$1
  local pid=${BENCH_PIDS[$index]}
  local expected_start=${BENCH_START_TIMES[$index]}
  local name=${BENCH_PROCESS_NAMES[$index]}
  local fields state current_start
  local attempts

  [[ $pid != 0 ]] || return 0
  fields=$(process_fields "$pid") || {
    wait "$pid" 2>/dev/null || true
    BENCH_PIDS[$index]=0
    return 0
  }
  read -r state current_start <<< "$fields"
  if [[ $current_start != "$expected_start" ]]; then
    printf 'refusing to signal reused PID %s formerly assigned to %s\n' "$pid" "$name" >&2
    BENCH_PIDS[$index]=0
    return 1
  fi

  kill -TERM "$pid"
  attempts=$((BENCH_STOP_TIMEOUT_SECONDS * 10))
  for ((i = 0; i < attempts; i++)); do
    fields=$(process_fields "$pid") || break
    read -r state current_start <<< "$fields"
    [[ $current_start == "$expected_start" ]] || break
    [[ $state == Z ]] && break
    sleep 0.1
  done

  if fields=$(process_fields "$pid"); then
    read -r state current_start <<< "$fields"
    if [[ $current_start == "$expected_start" && $state != Z ]]; then
      kill -KILL "$pid"
    fi
  fi
  wait "$pid" 2>/dev/null || true
  BENCH_PIDS[$index]=0
}

stop_named_process() {
  local name=$1
  local index
  for ((index = ${#BENCH_PIDS[@]} - 1; index >= 0; index--)); do
    if [[ ${BENCH_PROCESS_NAMES[$index]} == "$name" && ${BENCH_PIDS[$index]} != 0 ]]; then
      stop_process_index "$index"
      return
    fi
  done
}

cleanup_processes() {
  local status=$?
  local index
  trap - EXIT INT TERM
  for ((index = ${#BENCH_PIDS[@]} - 1; index >= 0; index--)); do
    stop_process_index "$index" || status=1
  done
  exit "$status"
}

install_cleanup_traps() {
  trap cleanup_processes EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
}

render_config() {
  local input=$1
  local output=$2
  shift 2
  python3 "$BENCHMARK_ROOT/scripts/tool.py" render "$input" "$output" "$@"
}

wait_for_http() {
  local url=$1
  local name=$2
  python3 "$BENCHMARK_ROOT/scripts/tool.py" wait-http "$url" 10 || die "$name did not become ready at $url"
}

check_http_payload() {
  local url=$1
  local expected_bytes=$2
  local name=$3
  python3 "$BENCHMARK_ROOT/scripts/tool.py" wait-http \
    "$url" 2 "$expected_bytes" || die "$name returned an invalid benchmark payload"
}
