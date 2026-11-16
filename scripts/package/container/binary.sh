#!/usr/bin/env bash
# Runs inside the binary build container (scripts/package/binary.sh): an image of the oldest supported glibc, so the
# binary runs on every distribution of the release. Builds with scripts/build-release-binaries.sh and fails when the
# binary needs a glibc symbol newer than the floor of families.sh.
#
# Environment: ARCH, RELEASE_VERSION, HOST_UID and, for the build script, PGO, BOLT, FLAVOUR, STAGE, RELEASE_TESTS,
# PACKAGE_JOBS, SOURCE_DATE_EPOCH, RELEASE_COMMIT.
# Mounts: /src.tar (the source), /package (scripts/package), /out (the result), optionally /cache (toolchain and cargo
# registry; also the build directory of a plain build, whose dependencies a later plain build reuses. A profile-guided build
# never reuses objects: each one is a function of the profile it is built with).
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "${here}/system.sh"
source "${here}/../families.sh"

src=/work/src
target=$(package_target "${ARCH}")
export CARGO_HOME=/cache/cargo RUSTUP_HOME=/cache/rustup
export PATH=${CARGO_HOME}/bin:${PATH}

install_build_dependencies() {
  refresh_index
  install_packages gcc gcc-c++ make cmake perl perl-IPC-Cmd perl-FindBin pkgconf-pkg-config python3 openssl \
    util-linux tar gzip findutils binutils httpd-tools
}

install_rust() {
  local url=https://static.rust-lang.org/rustup/dist/${target}/rustup-init download
  download=$(mktemp -d)
  curl --fail --silent --show-error --location --retry 5 --output "${download}/rustup-init" "${url}"
  curl --fail --silent --show-error --location --retry 5 --output "${download}/rustup-init.sha256" "${url}.sha256"
  (cd "${download}" && sha256sum --check --status rustup-init.sha256)
  chmod +x "${download}/rustup-init"
  "${download}/rustup-init" -y --no-modify-path --profile minimal --default-toolchain "${rust_toolchain}" \
    --component llvm-tools
  rm -rf -- "${download}"
}

pgo_work() {
  if [[ ${PGO} == 0 && -d /cache ]]; then echo /cache/plain-work; else echo /work/pgo; fi
}

build_binaries() {
  mkdir -p "${src}"
  tar --directory "${src}" --extract --file /src.tar
  (cd "${src}" && PGO_TARGET=${target} PGO_JOBS="${PACKAGE_JOBS}" PGO_WORK=$(pgo_work) GLIBC_FLOOR=${glibc_floor} \
    CARGO_INCREMENTAL=0 ./scripts/build-release-binaries.sh "${RELEASE_VERSION}" /out)
  chown -R "${HOST_UID}" /out
  [[ ! -d /cache ]] || chown -R "${HOST_UID}" /cache
}

install_build_dependencies
install_rust
build_binaries
