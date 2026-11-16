#!/usr/bin/env bash
# Fails when a binary needs a glibc symbol version newer than FLOOR: the binary then does not start on the oldest
# distribution glibc that the release promises (2.34: Debian 12, Ubuntu 22.04, RHEL-family 9, Amazon Linux 2023). The
# versions come from the version-needs section of the ELF file, so every symbol the dynamic linker resolves counts,
# whatever architecture the file is for.
#
#   glibc_floor.sh FLOOR BIN...
set -euo pipefail
export LC_ALL=C # readelf translates its headers

floor=${1:-}
[[ ${floor} =~ ^[0-9]+\.[0-9]+$ && $# -ge 2 ]] || { echo "usage: $0 FLOOR BIN..." >&2; exit 2; }
shift

highest_needed() {
  readelf -W -V "$1" | sed -n 's/.*Name: GLIBC_\([0-9][0-9.]*\).*/\1/p' | sort -V | tail -n 1
}

status=0
for binary in "$@"; do
  [[ -f ${binary} ]] || { echo "no such file: ${binary}" >&2; exit 2; }
  needed=$(highest_needed "${binary}")
  if [[ -n ${needed} && $(printf '%s\n%s\n' "${floor}" "${needed}" | sort -V | tail -n 1) != "${floor}" ]]; then
    printf '%s needs glibc %s, above the floor %s: build it in the oldest supported system\n' "${binary}" "${needed}" "${floor}" >&2
    status=1
  else
    printf '%s: glibc %s needed, floor %s\n' "${binary}" "${needed:-none}" "${floor}"
  fi
done
exit "${status}"
