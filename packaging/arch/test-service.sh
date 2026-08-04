#!/usr/bin/bash
set -euo pipefail

package_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
unit=${1:-"${package_dir}/oxiroute.service"}

[[ -f "${unit}" ]] || {
  printf 'service unit not found: %s\n' "${unit}" >&2
  exit 1
}

exec_start=
exec_reload=
while IFS= read -r line; do
  case "${line}" in
    ExecStart=*) exec_start=${line} ;;
    ExecReload=*) exec_reload=${line} ;;
  esac
done <"${unit}"

[[ "${exec_start}" == 'ExecStart=/usr/bin/oxiroute serve /etc/oxiroute/oxiroute.kdl' ]] || {
  printf 'unexpected ExecStart in %s: %s\n' "${unit}" "${exec_start}" >&2
  exit 1
}
[[ "${exec_reload}" == 'ExecReload=/bin/kill -HUP $MAINPID' ]] || {
  printf 'unexpected ExecReload in %s: %s\n' "${unit}" "${exec_reload}" >&2
  exit 1
}

printf 'verified service reload command: %s\n' "${exec_reload}"
