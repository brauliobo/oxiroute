#!/usr/bin/bash
# Runs package() of the source and binary recipes against a stub build tree and checks the installed file list, the
# modes and that the support files are installed verbatim. No build, download or installation takes place.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
root=$(mktemp -d)
trap 'rm -rf -- "${root}"' EXIT

expected_files='etc/oxiroute/oxiroute.env
etc/oxiroute/oxiroute.kdl
etc/oxiroute/oxiroute.lua
usr/bin/oxiroute
usr/bin/oxr
usr/lib/modules-load.d/oxiroute.conf
usr/lib/oxiroute/oxiroute-worker-launcher
usr/lib/systemd/system/oxiroute.service
usr/lib/sysusers.d/oxiroute.conf
usr/lib/tmpfiles.d/oxiroute.conf
usr/share/bash-completion/completions/oxiroute
usr/share/bash-completion/completions/oxr
usr/share/doc/oxiroute/arch-packaging.md
usr/share/doc/oxiroute/management-cli.md
usr/share/fish/vendor_completions.d/oxiroute.fish
usr/share/fish/vendor_completions.d/oxr.fish
usr/share/licenses/oxiroute/LICENSE
usr/share/man/man1/oxiroute.1
usr/share/man/man1/oxr.1
usr/share/zsh/site-functions/_oxiroute
usr/share/zsh/site-functions/_oxr'

stub_share() {
  local dir=$1 name
  mkdir -p -- "${dir}/share/bash-completion/completions" "${dir}/share/zsh/site-functions" \
    "${dir}/share/fish/vendor_completions.d" "${dir}/share/man/man1"
  for name in oxiroute oxr; do
    printf 'completion\n' >"${dir}/share/bash-completion/completions/${name}"
    printf 'completion\n' >"${dir}/share/zsh/site-functions/_${name}"
    printf 'completion\n' >"${dir}/share/fish/vendor_completions.d/${name}.fish"
    printf 'manual\n' >"${dir}/share/man/man1/${name}.1"
  done
}

# Stub build output for the layout each recipe's package() expects, relative to its "oxiroute-${pkgver}*" directory.
# SHARE=0 is a release tarball made before the completions and manual pages existed.
stub_tree() {
  local dir=$1 binary=$2 launcher=$3 share=$4
  mkdir -p -- "${dir}/$(dirname -- "${binary}")" "${dir}/$(dirname -- "${launcher}")" "${dir}/docs"
  printf '#!/usr/bin/bash\nprintf "generated %%s\\n" "$*"\n' >"${dir}/${binary}"
  chmod 755 "${dir}/${binary}"
  [[ ${share} == 0 ]] || stub_share "${dir}"
  printf 'launcher\n' >"${dir}/${launcher}"
  printf 'license\n' >"${dir}/LICENSE"
  printf 'management\n' >"${dir}/docs/MANAGEMENT_CLI.md"
}

check_package() {
  local label=$1 recipe=$2 tree=$3 binary=$4 launcher=$5 share=$6 work=${root}/$1-$6
  local srcdir=${work}/src pkgdir=${work}/pkg actual expected=${expected_files}
  [[ ${label} == source || ${share} == 1 ]] || expected=$(grep -Ev '^usr/share/(bash-completion|zsh|fish|man)/' <<< "${expected}")
  mkdir -p -- "${srcdir}" "${pkgdir}"
  cp -- "${recipe}"/oxiroute.* "${recipe}/README.md" "${srcdir}/"
  (
    source "${recipe}/PKGBUILD"
    tree=${tree//@VERSION@/${pkgver}}
    stub_tree "${srcdir}/${tree}" "${binary}" "${launcher}" "${share}"
    printf 'management\n' >"${srcdir}/management-cli-${pkgver}.md"
    objcopy() { :; }
    cd -- "${srcdir}"
    package
  )
  actual=$(cd -- "${pkgdir}" && find . -mindepth 1 ! -type d -printf '%P\n' | LC_ALL=C sort)
  [[ ${actual} == "${expected}" ]] || {
    printf '%s installs an unexpected file list:\n' "${label}" >&2
    diff -u <(printf '%s\n' "${expected}") <(printf '%s\n' "${actual}") >&2 || true
    exit 1
  }
  cmp -- "${recipe}/oxiroute.modules" "${pkgdir}/usr/lib/modules-load.d/oxiroute.conf"
  [[ $(stat -c %a "${pkgdir}/usr/lib/modules-load.d/oxiroute.conf") == 644 ]] || {
    printf '%s installs the kernel module list with a mode other than 0644\n' "${label}" >&2
    exit 1
  }
}

check_package source "${here}" 'oxiroute-@VERSION@' target/x86_64-unknown-linux-gnu/release/oxiroute \
  target/x86_64-unknown-linux-gnu/release/oxiroute-worker-launcher 1
for share in 1 0; do
  check_package binary "${here}/bin" 'oxiroute-@VERSION@-x86_64-unknown-linux-gnu' bin/oxiroute \
    lib/oxiroute/oxiroute-worker-launcher "${share}"
done
printf 'package file list tests passed\n'
