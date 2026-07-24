#!/usr/bin/env bash

set -euo pipefail
IFS=$'\n\t'

source "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"

mkdir -p -- "$GENERATED_ROOT/validation" "$GENERATED_ROOT/runs"
test_root=$(mktemp -d -- "$GENERATED_ROOT/validation/evidence-test.XXXXXX")
run_id="evidence-self-test-$$"
run="$GENERATED_ROOT/runs/$run_id"
report="$BENCHMARK_ROOT/reports/evidence-self-test-$$.json"
report_stem=${report##*/}
report_stem=${report_stem%.json}
evidence="$BENCHMARK_ROOT/reports/evidence/$report_stem"
nested="$BENCHMARK_ROOT/reports/evidence-self-test-nested-$$"

cleanup() {
  rm -rf -- "$test_root" "$run" "$evidence" "$nested"
  rm -f -- "$report"
  rmdir -- "$BENCHMARK_ROOT/reports/evidence" 2>/dev/null || true
}
trap cleanup EXIT

write_report() {
  printf '%s\n' \
    "{\"results\":{\"origin\":{\"requests_per_second\":[1],\"run_ids\":[\"$run_id\"]}},\"scenario\":{\"connections\":1},\"schema\":\"test-report\"}" \
    > "$report"
}

write_run() {
  rm -rf -- "$run"
  mkdir -p -- "$run/raw" "$run/config" "$run/logs"
  printf '%s\n' \
    '{"implementation":"oxiroute","provenance_file":"environment.json","schema":"oxiroute.local-v1.run.v2"}' \
    > "$run/run.json"
  printf '%s\n' \
    '{"git":{"dirty":false,"head":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","status_porcelain_v1":"","tracked_diff_sha256":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","untracked_files":[]},"loadgen_binary":{"path":"/loadgen","resolved_path":"/loadgen","sha256":"1111111111111111111111111111111111111111111111111111111111111111"},"oxiroute_binary":{"path":"/oxiroute","resolved_path":"/oxiroute","sha256":"2222222222222222222222222222222222222222222222222222222222222222"},"rust_toolchain":"1.87.0","schema":"oxiroute.local-v1.environment.v2","tools":{"cargo":{"path":"/cargo","resolved_path":"/cargo","sha256":"3333333333333333333333333333333333333333333333333333333333333333","version":"cargo verbose","version_arguments":["-Vv"]},"haproxy":{"path":"/haproxy","resolved_path":"/haproxy","sha256":"4444444444444444444444444444444444444444444444444444444444444444"},"nginx":{"path":"/nginx","resolved_path":"/nginx","sha256":"5555555555555555555555555555555555555555555555555555555555555555"},"rustc":{"path":"/rustc","resolved_path":"/rustc","sha256":"6666666666666666666666666666666666666666666666666666666666666666","version":"rustc verbose","version_arguments":["-Vv"]}}}' \
    > "$run/environment.json"
  printf '{"requests_per_second":1}\n' > "$run/summary-oxiroute.json"
  printf '{"requests_per_second":1}\n' > "$run/raw/loadgen-oxiroute.json"
  printf 'origin config\n' > "$run/config/nginx-origin.conf"
  printf 'proxy config\n' > "$run/config/oxiroute.lua"
  : > "$run/logs/origin-stdout.log"
  : > "$run/logs/origin-stderr.log"
  : > "$run/logs/oxiroute-stdout.log"
  : > "$run/logs/oxiroute-stderr.log"
  : > "$run/logs/loadgen-warmup-oxiroute.log"
  : > "$run/logs/loadgen-oxiroute.log"
}

expect_failure() {
  local label=$1
  shift
  if "$@" > /dev/null 2>&1; then
    die "$label was accepted"
  fi
}

write_report
write_run
"$BENCHMARK_ROOT/scripts/publish-evidence.sh" "$report" "$run" >/dev/null
python3 "$BENCHMARK_ROOT/scripts/tool.py" validate-reports "$BENCHMARK_ROOT/reports" >/dev/null
cp -- "$evidence/manifest.json" "$test_root/manifest.json"
cp -- "$evidence/SHA256SUMS" "$test_root/SHA256SUMS"
cp -- "$report" "$test_root/published-report.json"

rm -rf -- "$evidence"
write_report
python3 "$BENCHMARK_ROOT/scripts/tool.py" publish-evidence "$report" "$run" >/dev/null
cmp -s -- "$test_root/manifest.json" "$evidence/manifest.json" || \
  die "evidence manifest is not deterministic"
cmp -s -- "$test_root/SHA256SUMS" "$evidence/SHA256SUMS" || \
  die "evidence checksums are not deterministic"

python3 -c 'import json, pathlib, sys; p=pathlib.Path(sys.argv[1]); v=json.loads(p.read_text()); v["scenario"]["connections"] = 2; p.write_text(json.dumps(v) + "\n")' "$report"
expect_failure "modified report contents" \
  python3 "$BENCHMARK_ROOT/scripts/tool.py" validate-reports "$BENCHMARK_ROOT/reports"
cp -- "$test_root/published-report.json" "$report"

printf 'corruption\n' >> "$evidence/runs/$run_id/raw/loadgen-oxiroute.json"
expect_failure "modified archived artifact" \
  python3 "$BENCHMARK_ROOT/scripts/tool.py" validate-reports "$BENCHMARK_ROOT/reports"

rm -rf -- "$evidence"
write_report
write_run
python3 -c 'import json, pathlib, sys; p=pathlib.Path(sys.argv[1]); v=json.loads(p.read_text()); v["git"].update({"dirty": True, "status_porcelain_v1": " M source", "tracked_diff_sha256": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"}); p.write_text(json.dumps(v) + "\n")' "$run/environment.json"
expect_failure "dirty source provenance" \
  python3 "$BENCHMARK_ROOT/scripts/tool.py" publish-evidence "$report" "$run"

write_run
rm -- "$run/summary-oxiroute.json"
expect_failure "missing summary artifact" \
  python3 "$BENCHMARK_ROOT/scripts/tool.py" publish-evidence "$report" "$run"
write_run
rm -- "$run/raw/loadgen-oxiroute.json"
expect_failure "missing measured raw artifact" \
  python3 "$BENCHMARK_ROOT/scripts/tool.py" publish-evidence "$report" "$run"
write_run
: > "$run/config/oxiroute.lua"
expect_failure "empty rendered config" \
  python3 "$BENCHMARK_ROOT/scripts/tool.py" publish-evidence "$report" "$run"
write_run
rm -- "$run/logs/loadgen-oxiroute.log"
expect_failure "missing run log" \
  python3 "$BENCHMARK_ROOT/scripts/tool.py" publish-evidence "$report" "$run"

write_run
printf '{"run_ids":[],"schema":"test-report"}\n' > "$report"
expect_failure "empty report run IDs" \
  python3 "$BENCHMARK_ROOT/scripts/tool.py" publish-evidence "$report" "$run"
printf '{"run_ids":["other-run"],"schema":"test-report"}\n' > "$report"
expect_failure "mismatched report run IDs" \
  python3 "$BENCHMARK_ROOT/scripts/tool.py" publish-evidence "$report" "$run"

mkdir -p -- "$nested"
printf '%s\n' "{\"run_ids\":[\"$run_id\"],\"schema\":\"test-report\"}" > "$nested/report.json"
expect_failure "nested report path in Python" \
  python3 "$BENCHMARK_ROOT/scripts/tool.py" publish-evidence "$nested/report.json" "$run"
expect_failure "nested report path in shell" \
  "$BENCHMARK_ROOT/scripts/publish-evidence.sh" "$nested/report.json" "$run"

write_report
expect_failure "new report without evidence" \
  python3 "$BENCHMARK_ROOT/scripts/tool.py" validate-reports "$BENCHMARK_ROOT/reports"
printf '%s\n' \
  '{"evidence":{"reason":"not retained","schema":"oxiroute.benchmark-evidence-unavailable.v1","status":"historical_unavailable"},"schema":"test-report"}' \
  > "$report"
expect_failure "historical exception on a new report" \
  python3 "$BENCHMARK_ROOT/scripts/tool.py" validate-reports "$BENCHMARK_ROOT/reports"

cleanup
trap - EXIT
python3 "$BENCHMARK_ROOT/scripts/tool.py" validate-reports "$BENCHMARK_ROOT/reports" >/dev/null
printf 'evidence self-test passed\n'
