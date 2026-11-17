#!/usr/bin/env bash
# Runs inside the package container of a family (scripts/package/build.sh): assembles the family's package from the
# binary tarball of the release. Nothing is compiled here: the binary is the one the release publishes as a tarball.
#
# Environment: FAMILY, ARCH, RELEASE_VERSION, SOURCE_DATE_EPOCH, HOST_UID.
# Mounts: /src.tar (the source: recipes and layout files), /package (scripts/package), /out (the tarball in, the
# package out).
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "${here}/system.sh"
source "${here}/../families.sh"

work=/work
src=${work}/src
stage=${work}/stage
out=/out
target=$(package_target "${ARCH}")
tarball=${out}/$(tarball_stem "${RELEASE_VERSION}" "${target}" unwind).tar.gz
layout_files=(oxiroute.service oxiroute.sysusers oxiroute.tmpfiles oxiroute.modules oxiroute.env oxiroute.kdl oxiroute.lua)

install_packaging_tools() {
  refresh_index
  case ${FAMILY} in
    deb) install_packages dpkg-dev debhelper fakeroot binutils ;;
    *) install_packages rpm-build systemd-rpm-macros tar gzip binutils ;;
  esac
}

stage_files() {
  local file
  mkdir -p "${src}" "${stage}/payload" "${stage}/layout" "${stage}/docs"
  tar --directory "${src}" --extract --file /src.tar
  tar --directory "${stage}/payload" --strip-components=1 --extract --gzip --file "${tarball}"
  for file in "${layout_files[@]}"; do cp -- "${src}/packaging/arch/${file}" "${stage}/layout/"; done
  cp -- "${src}/docs/MANAGEMENT_CLI.md" "${src}/docs/user/INSTALL.md" "${stage}/docs/"
}

package_deb() {
  local tree=${work}/deb/oxiroute-$(package_version "${RELEASE_VERSION}") kind
  mkdir -p "${tree}"
  cp -a "${src}/packaging/deb/debian" "${tree}/debian"
  cp -a "${stage}" "${tree}/stage"
  for kind in service sysusers tmpfiles; do cp -- "${stage}/layout/oxiroute.${kind}" "${tree}/debian/"; done
  printf 'oxiroute (%s) unstable; urgency=medium\n\n  * Release %s.\n\n -- OxiRoute maintainers <noreply@example.invalid>  %s\n' \
    "$(package_version "${RELEASE_VERSION}")" "${RELEASE_VERSION}" "$(date -R -d "@${SOURCE_DATE_EPOCH}")" > "${tree}/debian/changelog"
  (cd "${tree}" && dpkg-buildpackage --build=binary --no-sign --no-check-builddeps)
  cp -- "${work}/deb/$(package_file "${FAMILY}" "${ARCH}" "${RELEASE_VERSION}")" "${out}/"
}

package_rpm() {
  local topdir=${work}/rpm
  mkdir -p "${topdir}"
  rpmbuild -bb "${src}/packaging/rpm/oxiroute.spec" --define "_topdir ${topdir}" --define "stage ${stage}" \
    --define "pkg_version $(package_version "${RELEASE_VERSION}")" --define 'use_source_date_epoch_as_buildtime 1' \
    --define 'clamp_mtime_to_source_date_epoch 1'
  cp -- "${topdir}/RPMS/${ARCH}/$(package_file "${FAMILY}" "${ARCH}" "${RELEASE_VERSION}")" "${out}/"
}

report_glibc_floor() {
  "${src}/scripts/verify/glibc_floor.sh" "${glibc_floor}" "${stage}/payload/bin/oxiroute" \
    "${stage}/payload/lib/oxiroute/oxiroute-worker-launcher"
}

export_assets() {
  local file
  file=$(package_file "${FAMILY}" "${ARCH}" "${RELEASE_VERSION}")
  cp -- "${stage}/payload/BUILD-INFO.json" "${out}/${file}.build.json"
  chown "${HOST_UID}" "${out}/${file}" "${out}/${file}.build.json"
}

install_packaging_tools
stage_files
report_glibc_floor
"package_$(package_format "${FAMILY}")"
export_assets
