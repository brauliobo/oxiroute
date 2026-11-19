#!/usr/bin/bash
# Runs package() of the source recipe, of the release package recipe (packaging/pacman) and of the oxiroute-bin recipe
# against stub inputs and checks the installed file list, the modes and that the support files are installed verbatim:
# the oxiroute-bin package must be exactly the files of the release package, and refuse a release package whose
# metadata, version or install script is not the one it declares. No build, download or installation takes place.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
root=$(mktemp -d)
trap 'rm -rf -- "${root}"' EXIT

source_files='etc/oxiroute/oxiroute.env
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
release_files=$(printf '%s\nusr/share/doc/oxiroute/BUILD-INFO.json\n' "${source_files}" | LC_ALL=C sort)
bin_version=$(source "${here}/bin/PKGBUILD" && printf '%s' "${pkgver}")

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

stub_binary() {
  printf '#!/usr/bin/bash\nprintf "generated %%s\\n" "$*"\n' >"$1"
  chmod 755 "$1"
}

listing() { (cd -- "$1" && find . -mindepth 1 ! -type d -printf '%P\n' | LC_ALL=C sort); }

require_listing() {
  local label=$1 pkgdir=$2 expected=$3
  [[ $(listing "${pkgdir}") == "${expected}" ]] || {
    printf '%s installs an unexpected file list:\n' "${label}" >&2
    diff -u <(printf '%s\n' "${expected}") <(listing "${pkgdir}") >&2 || true
    exit 1
  }
  cmp -- "${here}/oxiroute.modules" "${pkgdir}/usr/lib/modules-load.d/oxiroute.conf"
  [[ $(stat -c %a "${pkgdir}/usr/lib/modules-load.d/oxiroute.conf") == 644 ]] || {
    printf '%s installs the kernel module list with a mode other than 0644\n' "${label}" >&2
    exit 1
  }
}

package_source_recipe() {
  local srcdir=${root}/source/src pkgdir=${root}/source/pkg tree
  mkdir -p -- "${srcdir}" "${pkgdir}"
  cp -- "${here}"/oxiroute.* "${here}/README.md" "${srcdir}/"
  (
    source "${here}/PKGBUILD"
    tree=${srcdir}/oxiroute-${pkgver}
    mkdir -p -- "${tree}/docs" "${tree}/target/x86_64-unknown-linux-gnu/release"
    stub_binary "${tree}/target/x86_64-unknown-linux-gnu/release/oxiroute"
    printf 'launcher\n' >"${tree}/target/x86_64-unknown-linux-gnu/release/oxiroute-worker-launcher"
    printf 'license\n' >"${tree}/LICENSE"
    printf 'management\n' >"${tree}/docs/MANAGEMENT_CLI.md"
    objcopy() { :; }
    cd -- "${srcdir}"
    package
  )
  require_listing source "${pkgdir}" "${source_files}"
}

stage_release() {
  local stage=${root}/stage
  mkdir -p -- "${stage}/layout" "${stage}/docs" "${stage}/payload/bin" "${stage}/payload/lib/oxiroute"
  stub_binary "${stage}/payload/bin/oxiroute"
  stub_share "${stage}/payload"
  printf 'launcher\n' >"${stage}/payload/lib/oxiroute/oxiroute-worker-launcher"
  chmod 755 "${stage}/payload/lib/oxiroute/oxiroute-worker-launcher"
  printf 'license\n' >"${stage}/payload/LICENSE"
  printf '{"version": "%s"}\n' "${bin_version}" >"${stage}/payload/BUILD-INFO.json"
  cp -- "${here}"/oxiroute.{service,sysusers,tmpfiles,modules,env,kdl,lua} "${stage}/layout/"
  printf 'management\n' >"${stage}/docs/MANAGEMENT_CLI.md"
  cp -- "${here}/README.md" "${stage}/docs/arch-packaging.md"
}

package_release_recipe() {
  local pkgdir=${root}/release/pkg
  mkdir -p -- "${pkgdir}"
  stage_release
  (
    export OXIROUTE_STAGE=${root}/stage OXIROUTE_PACKAGE_VERSION=${bin_version}
    source "${here}/../pacman/PKGBUILD"
    cd -- "${root}"
    package
  )
  require_listing release "${pkgdir}" "${release_files}"
}

pkginfo_of_release_recipe() {
  (
    export OXIROUTE_STAGE=${root}/stage OXIROUTE_PACKAGE_VERSION=${1}
    source "${here}/../pacman/PKGBUILD"
    printf 'pkgname = %s\npkgver = %s-%s\n' "${pkgname}" "${pkgver}" "${pkgrel}"
    printf 'depend = %s\n' "${depends[@]}" "${@:2}"
    printf 'optdepend = %s\n' "${optdepends[@]}"
    printf 'backup = %s\n' "${backup[@]}"
  )
}

build_release_archive() {
  local archive=$1 version=$2 install=$3 content=${root}/archive
  rm -rf -- "${content}"
  cp -a -- "${root}/release/pkg" "${content}"
  pkginfo_of_release_recipe "${version}" "${@:4}" >"${content}/.PKGINFO"
  cp -- "${install}" "${content}/.INSTALL"
  printf 'mtree\n' >"${content}/.MTREE"
  printf 'buildinfo\n' >"${content}/.BUILDINFO"
  (cd -- "${content}" && bsdtar --zstd -cf "${archive}" .PKGINFO .INSTALL .MTREE .BUILDINFO etc usr)
}

package_bin_recipe() {
  local archive=$1 srcdir=${root}/bin/src pkgdir=${root}/bin/pkg
  rm -rf -- "${root:?}/bin"
  mkdir -p -- "${srcdir}" "${pkgdir}"
  (
    source "${here}/bin/PKGBUILD"
    startdir=${here}/bin
    cp -- "${archive}" "${srcdir}/${_package}"
    cd -- "${srcdir}"
    set -e
    package
  )
}

expect_bin_refused() {
  local label=$1
  shift
  local status
  build_release_archive "${root}/refused.pkg.tar.zst" "$@"
  set +e
  package_bin_recipe "${root}/refused.pkg.tar.zst" >/dev/null 2>&1
  status=$?
  set -e
  [[ ${status} != 0 ]] || { printf 'the oxiroute-bin recipe accepted %s\n' "${label}" >&2; exit 1; }
}

package_source_recipe
package_release_recipe

build_release_archive "${root}/release.pkg.tar.zst" "${bin_version}" "${here}/oxiroute.install"
package_bin_recipe "${root}/release.pkg.tar.zst"
diff -r -- "${root}/release/pkg" "${root}/bin/pkg" >&2 || { printf 'the oxiroute-bin package is not the release package\n' >&2; exit 1; }
[[ $(cd -- "${root}/bin/pkg" && find . -printf '%m %y %p %l\n' | sort) == $(cd -- "${root}/release/pkg" && find . -printf '%m %y %p %l\n' | sort) ]] ||
  { printf 'the oxiroute-bin package changed a mode or a link of the release package\n' >&2; exit 1; }
[[ $(readlink "${root}/bin/pkg/usr/bin/oxr") == oxiroute ]] || { printf 'oxr is not an alias of oxiroute\n' >&2; exit 1; }

printf 'another install script\n' >"${root}/other.install"
expect_bin_refused 'a release package with another install script' "${bin_version}" "${root}/other.install"
expect_bin_refused 'a release package of another version' 0.0.1 "${here}/oxiroute.install"
expect_bin_refused 'a release package with another dependency' "${bin_version}" "${here}/oxiroute.install" extra-library
printf 'package file list tests passed\n'
