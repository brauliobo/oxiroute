#!/usr/bin/env bash
# Tests scripts/verify/glibc_floor.sh against small dynamic executables whose glibc symbol versions are known.
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
check=${repo}/scripts/verify/glibc_floor.sh
root=$(mktemp -d)
trap 'rm -rf -- "${root}"' EXIT

die() { printf 'FAILED: %s\n' "$*" >&2; exit 1; }
needed_by() { "${check}" 99.0 "$1" | sed -n 's/.*: glibc \([0-9.]*\) needed.*/\1/p'; }
host_glibc=$(getconf GNU_LIBC_VERSION | cut -d' ' -f2)

printf 'int main(void){return 0;}\n' > "${root}/plain.c"
cc -o "${root}/plain" "${root}/plain.c"
plain=$(needed_by "${root}/plain")
[[ ${plain} =~ ^2\.[0-9]+$ ]] || die "no glibc version is reported for a dynamic executable: '${plain}'"
"${check}" "${plain}" "${root}/plain" > /dev/null || die "a binary that needs glibc ${plain} is refused at the floor ${plain}"
below=2.$((${plain#2.} - 1))
! "${check}" "${below}" "${root}/plain" 2> "${root}/err" || die "a binary that needs glibc ${plain} is accepted at the floor ${below}"
grep -q "needs glibc ${plain}, above the floor ${below}" "${root}/err" || die 'the refusal does not name the versions'
printf 'ok: the floor is compared with the highest symbol version the binary needs\n'

if [[ $(printf '%s\n2.36\n' "${host_glibc}" | sort -V | head -n 1) == 2.36 ]]; then
  printf '#include <stdlib.h>\nint main(void){char b[1];arc4random_buf(b,1);return 0;}\n' > "${root}/new.c"
  cc -o "${root}/new" "${root}/new.c"
  [[ $(needed_by "${root}/new") == 2.36 ]] || die 'a call of arc4random_buf does not need glibc 2.36'
  ! "${check}" 2.34 "${root}/plain" "${root}/new" 2> "${root}/err" || die 'a binary that needs glibc 2.36 is accepted at the floor 2.34'
  grep -q "${root}/new needs glibc 2.36" "${root}/err" || die 'the offending binary is not named'
  ! grep -q "${root}/plain" "${root}/err" || die 'a binary within the floor is named as an offender'
  printf 'ok: a symbol newer than the floor fails and names the binary\n'
else
  printf 'SKIP: the host glibc %s is older than 2.36, the newer-symbol case did not run\n' "${host_glibc}"
fi

! "${check}" 2> /dev/null || die 'no arguments were accepted'
! "${check}" 2.x "${root}/plain" 2> /dev/null || die 'a malformed floor was accepted'
printf 'glibc floor tests passed\n'
