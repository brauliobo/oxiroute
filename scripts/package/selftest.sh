#!/usr/bin/env bash
# Offline checks of scripts/package, the package recipes and the release workflow: the asset set, the matrix, the
# refusal of an incomplete or plain-built release, the wiring of the release workflows and the pins that must agree. No
# container is started.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "${here}/../.." && pwd)
source "${here}/families.sh"
assets=${here}/assets.sh
scratch=$(mktemp -d)
trap 'rm -rf -- "${scratch}"' EXIT
version=7.1.0

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

fake_release() {
  local dir=$1 asset
  mkdir -p "${dir}"
  while IFS= read -r asset; do
    case ${asset} in
      *.build.json) printf '{\n  "version": "%s",\n  "pgo": true\n}\n' "${version}" > "${dir}/${asset}" ;;
      *.tar.gz.sha256 | *.pkg.tar.zst.sha256) ;;
      *) printf '%s\n' "${asset}" > "${dir}/${asset}" ;;
    esac
  done < <("${assets}" list "${version}" | grep -v '^SHA256SUMS$')
  for asset in "${dir}"/*.tar.gz "${dir}"/*.pkg.tar.zst; do (cd "${dir}" && sha256sum -- "$(basename -- "${asset}")" > "$(basename -- "${asset}").sha256"); done
}

refused() { ! "$@" 2> "${scratch}/refusal"; }

test_complete_release_is_accepted_and_summed() {
  fake_release "${scratch}/ok"
  "${assets}" verify "${scratch}/ok" "${version}" || fail 'a complete release is refused'
  "${assets}" sums "${scratch}/ok" "${version}"
  (cd "${scratch}/ok" && sha256sum --check --quiet SHA256SUMS) || fail 'SHA256SUMS does not check'
  [[ $(wc -l < "${scratch}/ok/SHA256SUMS") == $("${assets}" list "${version}" | grep -vc '^SHA256SUMS$') ]] ||
    fail 'SHA256SUMS does not cover every asset exactly once'
}

test_every_missing_asset_is_refused() {
  local asset
  while IFS= read -r asset; do
    [[ ${asset} != SHA256SUMS ]] || continue
    fake_release "${scratch}/missing"
    rm -- "${scratch}/missing/${asset}"
    refused "${assets}" verify "${scratch}/missing" "${version}" || fail "a release without ${asset} is accepted"
    grep -q -- "${asset}" "${scratch}/refusal" || fail "the refusal does not name ${asset}"
    rm -rf -- "${scratch}/missing"
  done < <("${assets}" list "${version}")
}

test_plain_build_is_refused() {
  fake_release "${scratch}/plain"
  sed -i 's/"pgo": true/"pgo": false/' "${scratch}/plain/$(tarball_stem "${version}" "$(package_target x86_64)" unwind).build.json" \
    "${scratch}"/plain/*x86_64.rpm.build.json "${scratch}"/plain/*_amd64.deb.build.json "${scratch}"/plain/*.pkg.tar.zst.build.json
  refused "${assets}" verify "${scratch}/plain" "${version}" || fail 'a plain-built package is accepted'
  ALLOW_PLAIN=1 "${assets}" verify "${scratch}/plain" "${version}" || fail 'ALLOW_PLAIN does not accept a dry run'
}

test_stale_checksum_is_refused() {
  fake_release "${scratch}/stale"
  printf 'changed\n' >> "${scratch}/stale/oxiroute-${version}-aarch64-unknown-linux-gnu.tar.gz"
  refused "${assets}" verify "${scratch}/stale" "${version}" || fail 'a tarball that does not match its checksum is accepted'
  fake_release "${scratch}/stale-package"
  printf 'changed\n' >> "${scratch}/stale-package/$(package_file arch x86_64 "${version}")"
  refused "${assets}" verify "${scratch}/stale-package" "${version}" || fail 'an Arch package that does not match its checksum is accepted'
  grep -q 'pkg.tar.zst.sha256' "${scratch}/refusal" || fail 'the refusal does not name the checksum of the Arch package'
}

test_arch_package_is_an_x86_64_asset_with_a_checksum() {
  local list
  list=$("${assets}" list "${version}")
  grep -qx "oxiroute-${version}-1-x86_64.pkg.tar.zst" <<< "${list}" || fail 'no Arch package'
  grep -qx "oxiroute-${version}-1-x86_64.pkg.tar.zst.sha256" <<< "${list}" || fail 'no checksum of the Arch package'
  grep -qx "oxiroute-${version}-1-x86_64.pkg.tar.zst.build.json" <<< "${list}" || fail 'no build record of the Arch package'
  ! grep -q 'aarch64.*pkg.tar.zst' <<< "${list}" || fail 'an Arch package for aarch64'
  fake_release "${scratch}/notes"
  printf "build line\n" > "${scratch}/notes/pgo-note-x86_64.md"
  grep -q "oxiroute-${version}-1-x86_64.pkg.tar.zst" <<< "$("${assets}" notes "${scratch}/notes" "${version}")" ||
    fail 'the release notes do not list the Arch package'
}

test_release_assets_per_target() {
  local list
  list=$("${assets}" list "${version}")
  grep -qx "oxiroute-${version}-aarch64-unknown-linux-gnu.tar.gz" <<< "${list}" || fail 'no aarch64 tarball'
  grep -qx "oxiroute-${version}-x86_64-unknown-linux-gnu-abort.tar.gz" <<< "${list}" || fail 'no abort tarball'
  ! grep -q 'aarch64.*\(abort\|x86-64-v3\)' <<< "${list}" || fail 'a flavour other than unwind on aarch64'
  ! grep -q 'x86-64-v3' <<< "${list}" || fail 'the x86-64-v3 flavour is expected by default'
  list=$(X86_64_FLAVOURS='unwind abort v3' "${assets}" list "${version}")
  grep -q 'x86-64-v3.tar.gz$' <<< "${list}" || fail 'v3 is not selectable'
}

matrix_size() { python3 -I -c 'import json,sys; print(len(json.load(sys.stdin)["include"]))'; }

test_matrices() {
  [[ $("${here}/matrix.sh" assemble x86_64 | matrix_size) == 4 ]] || fail 'the x86_64 assemble matrix is not one job per family'
  [[ $("${here}/matrix.sh" assemble aarch64 | matrix_size) == 3 ]] || fail 'the aarch64 assemble matrix has a family that is not released for it'
  [[ $("${here}/matrix.sh" install x86_64 | matrix_size) == 9 ]] || fail 'the x86_64 install matrix is not one job per image of every family'
  [[ $("${here}/matrix.sh" install aarch64 | matrix_size) == 8 ]] || fail 'the aarch64 install matrix is not one job per image of its families'
  grep -q '"family":"arch","image":"archlinux:latest"' <<< "$("${here}/matrix.sh" install x86_64)" || fail 'Arch Linux is not tested'
  ! grep -q '"family":"arch"' <<< "$("${here}/matrix.sh" assemble aarch64)" || fail 'the Arch package is assembled for aarch64'
  refused "${here}/matrix.sh" assemble || fail 'an assemble matrix without an architecture is accepted'
  [[ $("${here}/matrix.sh" any extras) == true && $(ABORT=0 "${here}/matrix.sh" any extras) == false ]] || fail 'ABORT does not select the extras'
  [[ $(ABORT=0 V3=1 "${here}/matrix.sh" extras) == '{"include":[{"flavour":"v3"}]}' ]] || fail 'V3 is not an extra flavour'
  [[ $(ABORT=0 BOLT=0 "${here}/matrix.sh" any tests) == false ]] || fail 'a dry run runs the workspace tests'
  [[ $(BOLT=0 "${here}/matrix.sh" tests) == '{"include":[{"flavour":"abort"}]}' ]] || fail 'the abort flavour is not tested'
  [[ $("${here}/matrix.sh" tests | matrix_size) == 2 ]] || fail 'the BOLT layout is not tested'
  grep -q '"image":"rockylinux/rockylinux:9"' <<< "$("${here}/matrix.sh" install x86_64)" || fail 'Rocky Linux 9 is not tested'
  refused "${here}/matrix.sh" bogus || fail 'an unknown matrix is accepted'
}

test_glibc_floor_is_the_lowest_of_the_images() {
  [[ ${binary_image} == almalinux:9 && ${glibc_floor} == 2.34 ]] || fail 'the binary is not built on the el9 glibc'
  grep -q 'GLIBC_FLOOR=${glibc_floor}' "${here}/layout.sh" || fail 'the layout stage does not check the floor'
  grep -q 'GLIBC_FLOOR=${glibc_floor}' "${here}/container/binary.sh" || fail 'the container build does not check the floor'
}

test_one_binary_per_architecture() {
  fake_release "${scratch}/two"
  printf '{\n  "version": "%s",\n  "pgo": true,\n  "another": 1\n}\n' "${version}" \
    > "${scratch}/two/$(package_file el9 aarch64 "${version}").build.json"
  refused "${assets}" verify "${scratch}/two" "${version}" || fail 'a package of another binary than its tarball is accepted'
  grep -q 'el9.aarch64.rpm' "${scratch}/refusal" || fail 'the refusal does not name the package'
}

test_file_names_round_trip() {
  local family arch name
  while read -r family arch; do
    name=$(package_file "${family}" "${arch}" 7.0.0-rc.1)
    [[ $(file_version "${family}" "${name}") == 7.0.0-rc.1 ]] || fail "${name} does not give its version back"
  done < <(package_pairs)
  [[ $(package_file deb x86_64 7.0.0) == oxiroute_7.0.0_amd64.deb ]] || fail 'deb file name'
  [[ $(package_file el9 aarch64 7.0.0) == oxiroute-7.0.0-1.el9.aarch64.rpm ]] || fail 'el9 file name'
  [[ $(package_file amzn2023 x86_64 7.0.0) == oxiroute-7.0.0-1.amzn2023.x86_64.rpm ]] || fail 'amzn2023 file name'
  [[ $(package_file arch x86_64 7.0.0) == oxiroute-7.0.0-1-x86_64.pkg.tar.zst ]] || fail 'arch file name'
  [[ $(package_file arch x86_64 7.0.0-rc.1) == oxiroute-7.0.0_rc.1-1-x86_64.pkg.tar.zst ]] || fail 'arch pre-release file name'
  [[ $(package_glob arch x86_64) == oxiroute-*-1-x86_64.pkg.tar.zst ]] || fail 'arch glob'
}

test_workflows_are_pinned() {
  local pins workflows=("${repo}"/.github/workflows/release*.yml)
  pins=$(sed -n 's/^ *toolchain: *//p' "${workflows[@]}" | sort -u)
  [[ ${pins} == "${rust_toolchain}" ]] || fail "the release workflows install ${pins}, scripts/package/families.sh ${rust_toolchain}"
  ! grep -hE 'uses: [^ ]+@[^ ]+' "${workflows[@]}" | grep -qvE '@[0-9a-f]{40} #' || fail 'an action is not pinned by commit SHA'
  grep -q "runner: $(runner_of x86_64)" "${repo}/.github/workflows/release.yml" || fail 'x86_64 is not on its runner'
  grep -q "runner: $(runner_of aarch64)" "${repo}/.github/workflows/release.yml" || fail 'aarch64 is not on its runner'
}

# The needs line of a job of the release workflow.
needs_of() {
  awk -v job="$1" '$0 == "  " job ":" { on = 1; next } on && /^  [a-z]/ { exit } on && /^    needs:/ { print }' "${repo}/.github/workflows/release.yml"
}

test_release_workflow_wiring() {
  local file=${repo}/.github/workflows/release.yml job
  [[ $(needs_of packages-x86_64) == *binary-x86_64* && $(needs_of packages-x86_64) != *aarch64* ]] ||
    fail 'packages-x86_64 does not start on binary-x86_64 alone'
  [[ $(needs_of packages-aarch64) == *binary-aarch64* && $(needs_of packages-aarch64) != *x86_64* ]] ||
    fail 'packages-aarch64 does not start on binary-aarch64 alone'
  [[ $(needs_of binary-x86_64) != *archive* && $(needs_of binary-aarch64) != *archive* ]] || fail 'a binary waits for the archive job'
  for job in selftest archive binary-x86_64 binary-aarch64 binary-extras binary-tests packages-x86_64 packages-aarch64; do
    sed -n '/^  publish:/,$p' "${file}" | grep -q "^      - ${job}$" || fail "publish does not need ${job}"
  done
  grep -q "cancel-in-progress: \${{ github.ref_type != 'tag' && github.event_name != 'workflow_dispatch' }}" "${file}" ||
    fail 'a release run can be cancelled'
  grep -q 'assets sums dist' "${file}" && ! grep -q ALLOW_PLAIN "${repo}"/.github/workflows/release*.yml || fail 'publish does not refuse a plain build'
}

test_profile_is_restored_never_silently_skipped() {
  local binary=${repo}/.github/workflows/release-binary.yml profile=${repo}/.github/workflows/release-profile.yml
  grep -q 'fail-on-cache-miss: true' "${binary}" || fail 'the binary job can build without the profile it was told to use'
  grep -q 'PGO_PROFILE_HOST' "${binary}" || fail 'the binary job does not hand the restored profile to the build'
  grep -q 'uses: ./.github/workflows/release-profile.yml' "${binary}" || fail 'the binary job does not use the profile workflow'
  grep -q 'key: ${{ needs.lookup.outputs.exact }}' "${profile}" || fail 'the trained profile is not saved under the exact key that is looked up'
  grep -q 'cron:' "${repo}/.github/workflows/profile-refresh.yml" || fail 'nothing refreshes the profile of main'
  sed -n '/^on:/,/^permissions:/p' "${repo}/.github/workflows/profile-refresh.yml" | grep -q -- '- Cargo.toml' ||
    fail 'a push that bumps the version does not refresh the profile'
  grep -q 'reuse_newest: false' "${repo}/.github/workflows/profile-refresh.yml" || fail 'the refresh would reuse an old profile instead of training a new key'
  grep -q 'reuse_newest: true' "${binary}" || fail 'a release build does not accept the newest profile of its version'
}

test_profile_keys_name_architecture_and_flavour() {
  local keys
  keys=$(for arch in "${package_arches[@]}"; do for flavour in unwind abort; do "${here}/profile.sh" key "${arch}" "${flavour}"; done; done)
  [[ $(sort -u <<< "${keys}" | wc -l) == 4 ]] || fail "the profile keys of the architectures and flavours are not distinct: ${keys}"
  [[ $("${here}/profile.sh" key x86_64 unwind) == "$("${here}/profile.sh" prefix x86_64 unwind)"* ]] || fail 'the key is not under the prefix of its version'
  [[ $("${here}/profile.sh" prefix x86_64 unwind) == *"-v$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "${repo}/Cargo.toml" | head -1)-" ]] ||
    fail 'the prefix does not name the workspace version'
  [[ $("${here}/profile.sh" prefix x86_64 unwind) != "$("${here}/profile.sh" prefix x86_64 abort)" ]] || fail 'the flavours share a prefix'

}

test_recipes_carry_no_version() {
  local current
  current=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "${repo}/Cargo.toml" | head -n 1)
  ! grep -rnwF -e "${current}" -e 7.0.0 "${repo}/packaging/deb" "${repo}/packaging/rpm" "${repo}/packaging/pacman" "${repo}/scripts/package/container" \
    "${repo}/scripts/package/build.sh" "${repo}/scripts/package/test.sh" || fail 'a literal release version in a recipe or script'
}

test_recipe_paths_cover_the_workflow_filter() {
  local path
  while read -r path; do
    [[ ${path} == packaging/* ]] && continue
    grep -Eq "${recipe_paths_re}" <<< "${path/\*\*/x}" || fail "${path} runs the release workflow but is not in recipe_paths_re"
  done < <(sed -n '/^  pull_request:/,/^  workflow_dispatch:/s/^ *- //p' "${repo}/.github/workflows/release.yml")
}

test_layout_comes_from_packaging_arch() {
  local file
  sh -n "${repo}/packaging/arch/oxiroute.ensure-token" || fail 'oxiroute.ensure-token has invalid shell syntax'
  bash -n "${repo}/packaging/install-staged.sh" || fail 'install-staged.sh has invalid bash syntax'
  for file in $(sed -n 's/^layout_files=(\(.*\))/\1/p' "${here}/container/package.sh"); do
    [[ -f ${repo}/packaging/arch/${file} ]] || fail "packaging/arch/${file} does not exist"
    grep -q "${file}" "${repo}/packaging/install-staged.sh" || fail "${file} is not installed by packaging/install-staged.sh"
    grep -q "${file}" "${repo}/packaging/arch/PKGBUILD" || fail "${file} is not in the Arch source recipe"
  done
  for file in packaging/deb/debian/rules packaging/rpm/oxiroute.spec packaging/pacman/PKGBUILD; do
    grep -q 'install-staged\.sh' "${repo}/${file}" || fail "${file} does not use packaging/install-staged.sh"
  done
  for file in packaging/arch/oxiroute.install packaging/deb/debian/oxiroute.postinst packaging/rpm/oxiroute.spec; do
    grep -q 'ensure-management-token' "${repo}/${file}" || fail "${file} does not call ensure-management-token"
    ! grep -q 'token create' "${repo}/${file}" || fail "${file} duplicates token creation instead of calling ensure-management-token"
  done
  grep -Eq "${recipe_paths_re}" <<< packaging/install-staged.sh || fail 'install-staged.sh does not start the package build'
  grep -Eq "${recipe_paths_re}" <<< packaging/arch/oxiroute.ensure-token || fail 'oxiroute.ensure-token does not start the package build'
  [[ $(grep -c '%config(noreplace)' "${repo}/packaging/rpm/oxiroute.spec") == 3 ]] || fail 'the rpm does not mark the 3 configuration files'
  ! grep -rn 'enable\|restart' "${repo}/packaging/deb/debian/rules" | grep -v 'no-enable' || fail 'the deb enables a service'
}

test_arch_release_workflow_wiring() {
  local packages=${repo}/.github/workflows/release-packages.yml
  [[ $(grep -c 'dist/\*\.pkg\.tar\.zst$' "${packages}") == 2 ]] || fail 'the Arch package is not both uploaded and attested'
  grep -q 'dist/\*\.pkg\.tar\.zst\.sha256$' "${packages}" || fail 'the checksum of the Arch package is not uploaded'
  grep -q 'matrix assemble "${{ inputs.arch }}"' "${packages}" || fail 'the assemble matrix does not depend on the architecture'
  grep -q 'matrix install "${{ inputs.arch }}"' "${packages}" || fail 'the install matrix does not depend on the architecture'
  grep -Eq "${recipe_paths_re}" <<< packaging/pacman/PKGBUILD || fail 'the Arch recipe does not start the package build'
  grep -Eq "${recipe_paths_re}" <<< packaging/arch/oxiroute.install || fail 'the install script does not start the package build'
  ! grep -Eq "${recipe_paths_re}" <<< packaging/arch/bin/PKGBUILD || fail 'the AUR recipe starts the package build'
}

test_oxiroute_bin_installs_the_release_package() {
  local bin_version bin_package bin_source
  bin_version=$(source "${repo}/packaging/arch/bin/PKGBUILD" && printf '%s' "${pkgver}")
  bin_package=$(source "${repo}/packaging/arch/bin/PKGBUILD" && printf '%s' "${_package}")
  bin_source=$(source "${repo}/packaging/arch/bin/PKGBUILD" && printf '%s' "${source[0]%%::*}")
  [[ ${bin_package} == "$(package_file arch x86_64 "${bin_version}")" ]] || fail 'oxiroute-bin does not name the release package'
  [[ ${bin_source} == "${bin_package}" ]] || fail 'oxiroute-bin downloads a file other than the release package'
  [[ $(OXIROUTE_PACKAGE_VERSION=${bin_version} OXIROUTE_STAGE=/stage; source "${repo}/packaging/pacman/PKGBUILD" && printf '%s' "${pkgname}") == oxiroute ]] ||
    fail 'the release package is not named oxiroute'
}

test_first_release_with_packages_has_no_upgrade_source() {
  mkdir -p "${scratch}/bin"
  printf '%s\n' '#!/usr/bin/env bash' \
    '[[ $2 == list ]] && { echo v6.5.2; exit 0; }' \
    '[[ $2 == view ]] && { echo oxiroute-6.5.2.tar.gz; exit 0; }' \
    'exit 1' > "${scratch}/bin/gh"
  chmod +x "${scratch}/bin/gh"
  local found
  found=$(PATH="${scratch}/bin:${PATH}" "${here}/previous.sh" deb x86_64 v7.1.0 "${scratch}/previous") ||
    fail 'a release history without packages fails the lookup of the previous package'
  [[ -z ${found} ]] || fail "a release without packages was offered as upgrade source: ${found}"
}

test_complete_release_is_accepted_and_summed
test_arch_package_is_an_x86_64_asset_with_a_checksum
test_arch_release_workflow_wiring
test_oxiroute_bin_installs_the_release_package
test_first_release_with_packages_has_no_upgrade_source
test_every_missing_asset_is_refused
test_plain_build_is_refused
test_stale_checksum_is_refused
test_release_assets_per_target
test_one_binary_per_architecture
test_matrices
test_glibc_floor_is_the_lowest_of_the_images
test_file_names_round_trip
test_workflows_are_pinned
test_release_workflow_wiring
test_profile_is_restored_never_silently_skipped
test_profile_keys_name_architecture_and_flavour
test_recipes_carry_no_version
test_recipe_paths_cover_the_workflow_filter
test_layout_comes_from_packaging_arch
printf 'package selftest passed\n'
