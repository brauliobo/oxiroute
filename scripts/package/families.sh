#!/usr/bin/env bash
# Sourced: the one table of the release package families, their container images and the name of every release asset.

package_families=(deb el9 amzn2023)
package_arches=(x86_64 aarch64)
rust_toolchain=1.97.1
glibc_floor=2.34
binary_image=almalinux:9
version_re='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$'
# The files a package depends on: a change to one of them is what makes a pull request build and test the packages.
recipe_paths_re='^(packaging/(deb|rpm|smoke)/|packaging/arch/oxiroute\.(service|sysusers|tmpfiles|modules|env|kdl|lua)$|scripts/package/'
recipe_paths_re+='|scripts/(build-release-binaries|verify-baseline-isa|verify-binary-paths|verify/glibc_floor)\.sh$|scripts/lib/reject-isa-flags\.sh$'
recipe_paths_re+='|benchmarks/scripts/pgo\.sh$|docs/(user/INSTALL|MANAGEMENT_CLI)\.md$|\.github/workflows/release(-binary|-packages)?\.yml$)'

require_version() {
  [[ $1 =~ ${version_re} ]] || { printf 'not a release version: %s\n' "$1" >&2; return 1; }
}

require_member() {
  local what=$1 value=$2 member
  shift 2
  for member in "$@"; do [[ ${member} == "${value}" ]] && return 0; done
  printf 'unknown %s %s (one of: %s)\n' "${what}" "${value}" "$*" >&2
  return 1
}

host_arch() {
  local machine
  machine=$(uname -m)
  printf '%s\n' "${machine/arm64/aarch64}"
}

container_platform() {
  case $1 in
    x86_64) echo linux/amd64 ;;
    aarch64) echo linux/arm64 ;;
  esac
}

runner_of() {
  case $1 in
    x86_64) echo ubuntu-24.04 ;;
    aarch64) echo ubuntu-24.04-arm ;;
  esac
}

package_target() { printf '%s-unknown-linux-gnu\n' "$1"; }

# The image that assembles the family's package: the family's own packaging tools.
build_image() {
  case $1 in
    deb) echo debian:12 ;;
    el9) echo almalinux:9 ;;
    amzn2023) echo amazonlinux:2023 ;;
  esac
}

# Fresh images of the same architecture that install the package: every image of the family it is promised to run on.
test_images() {
  case $1 in
    deb) echo debian:12 debian:13 ubuntu:22.04 ubuntu:24.04 ;;
    el9) echo almalinux:9 rockylinux/rockylinux:9 almalinux:10 ;;
    amzn2023) echo amazonlinux:2023 ;;
  esac
}

package_format() {
  case $1 in
    deb) echo deb ;;
    *) echo rpm ;;
  esac
}

# A pre-release suffix sorts below the release in both formats when it follows a tilde instead of a hyphen.
package_version() { printf '%s\n' "${1//-/\~}"; }

package_file() {
  local family=$1 arch=$2 version
  version=$(package_version "$3")
  case ${family} in
    deb) printf 'oxiroute_%s_%s.deb\n' "${version}" "$([[ ${arch} == x86_64 ]] && echo amd64 || echo arm64)" ;;
    el9) printf 'oxiroute-%s-1.el9.%s.rpm\n' "${version}" "${arch}" ;;
    amzn2023) printf 'oxiroute-%s-1.amzn2023.%s.rpm\n' "${version}" "${arch}" ;;
  esac
}

# The assets of the family and architecture, as a shell pattern, whatever the version.
package_glob() {
  local family=$1 arch=$2
  case ${family} in
    deb) printf 'oxiroute_*_%s.deb\n' "$([[ ${arch} == x86_64 ]] && echo amd64 || echo arm64)" ;;
    el9) printf 'oxiroute-*-1.el9.%s.rpm\n' "${arch}" ;;
    amzn2023) printf 'oxiroute-*-1.amzn2023.%s.rpm\n' "${arch}" ;;
  esac
}

file_version() {
  local name
  name=$(basename -- "$2")
  case $1 in
    deb) name=${name#oxiroute_}; name=${name%%_*} ;;
    *) name=${name#oxiroute-}; name=${name%%-1.*} ;;
  esac
  printf '%s\n' "${name//\~/-}"
}

flavour_suffix() {
  case $1 in
    unwind) echo ;;
    abort) echo -abort ;;
    v3) echo -x86-64-v3 ;;
  esac
}

tarball_stem() { printf 'oxiroute-%s-%s%s\n' "$1" "$2" "$(flavour_suffix "$3")"; }

# The flavours of a target: only the x86_64 build has the fail-fast and the x86-64-v3 flavours.
target_flavours() {
  case $1 in
    x86_64-unknown-linux-gnu) echo "${X86_64_FLAVOURS:-unwind abort}" ;;
    aarch64-unknown-linux-gnu) echo unwind ;;
  esac
}

release_targets() {
  local arch
  for arch in "${package_arches[@]}"; do package_target "${arch}"; done
}

# Every file the release carries but SHA256SUMS, one per line.
release_assets() {
  local version=$1 target flavour stem family arch file
  printf 'oxiroute-%s.tar.gz\nSOURCE-PROVENANCE.json\n' "${version}"
  for target in $(release_targets); do
    for flavour in $(target_flavours "${target}"); do
      stem=$(tarball_stem "${version}" "${target}" "${flavour}")
      printf '%s.tar.gz\n%s.tar.gz.sha256\n%s.build.json\n' "${stem}" "${stem}" "${stem}"
    done
  done
  for family in "${package_families[@]}"; do
    for arch in "${package_arches[@]}"; do
      file=$(package_file "${family}" "${arch}" "${version}")
      printf '%s\n%s.build.json\n' "${file}" "${file}"
    done
  done
}
