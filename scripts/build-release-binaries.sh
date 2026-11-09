#!/usr/bin/env bash
# Builds the release binaries of one target and packs them as a tarball with checksum, build record and release-note
# line. Profile-guided by default (benchmarks/scripts/pgo.sh: instrumented build, training replay, merge, profile-use
# build); PGO=0 builds plain. A training or profile failure fails the script: there is no silent plain fallback.
#
#   build-release-binaries.sh VERSION OUT_DIR
#
# Environment: PGO (1 or 0, default 1), PGO_TARGET (default x86_64-unknown-linux-gnu), PGO_WORK and the other
# benchmarks/scripts/pgo.sh variables, PGO_KEEP_INSTRUMENTED=1 (keep the instrumented build; it is removed to save disk), SOURCE_DATE_EPOCH (default: commit time of HEAD).
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
version=${1:-}
out=${2:-}
[[ -n ${version} && -n ${out} && ${version} != -* && ${out} != -* ]] || { echo "usage: $0 VERSION OUT_DIR" >&2; exit 2; }
target=${PGO_TARGET:-x86_64-unknown-linux-gnu}
pgo=${PGO:-1}
[[ ${pgo} == 0 || ${pgo} == 1 ]] || { echo "PGO must be 0 or 1" >&2; exit 2; }
pgo_script=${repo}/benchmarks/scripts/pgo.sh
work=${PGO_WORK:-/dev/shm/oxiroute-pgo}
profile=${PGO_PROFILE:-${work}/oxiroute.profdata}
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "${repo}" log -1 --format=%ct HEAD)}
name=oxiroute-${version}-${target}
out=$(realpath -m -- "${out}")
mkdir -p -- "${out}"

if [[ ${pgo} == 1 ]]; then
  "${pgo_script}" train
  [[ ${PGO_KEEP_INSTRUMENTED:-0} == 1 ]] || rm -rf -- "${work}/target-gen"
  "${pgo_script}" build
  release=${CARGO_TARGET_DIR:-${work}/target-use}/${target}/release
  pgo_json=true
  note="built with profile-guided optimization (trained on the pinned compiler by the release workflow; \`${name}.build.json\` records it)"
  profile_sha256="\"$(sha256sum "${profile}" | cut -d' ' -f1)\""
else
  "${pgo_script}" plain
  release=${CARGO_TARGET_DIR:-${work}/target-plain}/${target}/release
  pgo_json=false
  note="a plain build without profile-guided optimization (the release workflow was started with PGO disabled)"
  profile_sha256=null
fi
"${pgo_script}" smoke "${release}/oxiroute"

stage=$(mktemp -d)
trap 'rm -rf -- "${stage}"' EXIT
root=${stage}/${name}
install -Dm755 "${release}/oxiroute" "${root}/bin/oxiroute"
ln -s oxiroute "${root}/bin/oxr"
install -Dm755 "${release}/oxiroute-worker-launcher" "${root}/lib/oxiroute/oxiroute-worker-launcher"
install -Dm644 "${repo}/LICENSE" "${root}/LICENSE"
cat > "${root}/BUILD-INFO.json" <<JSON
{
  "schema": "oxiroute.release-binary.v1",
  "version": "${version}",
  "target": "${target}",
  "commit": "$(git -C "${repo}" rev-parse HEAD)",
  "rustc": "$(rustc --version)",
  "pgo": ${pgo_json},
  "pgo_profile_sha256": ${profile_sha256}
}
JSON

tar --directory="${stage}" --sort=name --mtime="@${SOURCE_DATE_EPOCH}" --owner=0 --group=0 --numeric-owner \
  --create "${name}" | gzip -n > "${out}/${name}.tar.gz"
(cd "${out}" && sha256sum "${name}.tar.gz" > "${name}.tar.gz.sha256")
cp -- "${root}/BUILD-INFO.json" "${out}/${name}.build.json"
printf "The \`%s\` binaries are %s.\n" "${target}" "${note}" > "${out}/pgo-note-${target}.md"
ls -l -- "${out}"
