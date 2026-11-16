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
      *.tar.gz.sha256) ;;
      *) printf '%s\n' "${asset}" > "${dir}/${asset}" ;;
    esac
  done < <("${assets}" list "${version}" | grep -v '^SHA256SUMS$')
  for asset in "${dir}"/*.tar.gz; do (cd "${dir}" && sha256sum -- "$(basename -- "${asset}")" > "$(basename -- "${asset}").sha256"); done
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
    "${scratch}"/plain/*x86_64.rpm.build.json "${scratch}"/plain/*_amd64.deb.build.json
  refused "${assets}" verify "${scratch}/plain" "${version}" || fail 'a plain-built package is accepted'
  ALLOW_PLAIN=1 "${assets}" verify "${scratch}/plain" "${version}" || fail 'ALLOW_PLAIN does not accept a dry run'
}

test_stale_checksum_is_refused() {
  fake_release "${scratch}/stale"
  printf 'changed\n' >> "${scratch}/stale/oxiroute-${version}-aarch64-unknown-linux-gnu.tar.gz"
  refused "${assets}" verify "${scratch}/stale" "${version}" || fail 'a tarball that does not match its checksum is accepted'
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
  [[ $("${here}/matrix.sh" assemble | matrix_size) == 3 ]] || fail 'the assemble matrix is not one job per family'
  [[ $("${here}/matrix.sh" install | matrix_size) == 8 ]] || fail 'the install matrix is not one job per image of every family'
  [[ $("${here}/matrix.sh" any extras) == true && $(ABORT=0 "${here}/matrix.sh" any extras) == false ]] || fail 'ABORT does not select the extras'
  [[ $(ABORT=0 V3=1 "${here}/matrix.sh" extras) == '{"include":[{"flavour":"v3"}]}' ]] || fail 'V3 is not an extra flavour'
  [[ $(ABORT=0 BOLT=0 "${here}/matrix.sh" any tests) == false ]] || fail 'a dry run runs the workspace tests'
  [[ $(BOLT=0 "${here}/matrix.sh" tests) == '{"include":[{"flavour":"abort"}]}' ]] || fail 'the abort flavour is not tested'
  [[ $("${here}/matrix.sh" tests | matrix_size) == 2 ]] || fail 'the BOLT layout is not tested'
  grep -q '"image":"rockylinux/rockylinux:9"' <<< "$("${here}/matrix.sh" install)" || fail 'Rocky Linux 9 is not tested'
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
  for family in "${package_families[@]}"; do
    for arch in "${package_arches[@]}"; do
      name=$(package_file "${family}" "${arch}" 7.0.0-rc.1)
      [[ $(file_version "${family}" "${name}") == 7.0.0-rc.1 ]] || fail "${name} does not give its version back"
    done
  done
  [[ $(package_file deb x86_64 7.0.0) == oxiroute_7.0.0_amd64.deb ]] || fail 'deb file name'
  [[ $(package_file el9 aarch64 7.0.0) == oxiroute-7.0.0-1.el9.aarch64.rpm ]] || fail 'el9 file name'
  [[ $(package_file amzn2023 x86_64 7.0.0) == oxiroute-7.0.0-1.amzn2023.x86_64.rpm ]] || fail 'amzn2023 file name'
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
  for job in archive binary-x86_64 binary-aarch64 binary-extras binary-tests packages-x86_64 packages-aarch64; do
    sed -n '/^  publish:/,$p' "${file}" | grep -q "^      - ${job}$" || fail "publish does not need ${job}"
  done
  grep -q "cancel-in-progress: \${{ github.ref_type != 'tag' && github.event_name != 'workflow_dispatch' }}" "${file}" ||
    fail 'a release run can be cancelled'
  grep -q 'assets sums dist' "${file}" && ! grep -q ALLOW_PLAIN "${repo}"/.github/workflows/release*.yml || fail 'publish does not refuse a plain build'
}

test_recipes_carry_no_version() {
  local current
  current=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "${repo}/Cargo.toml" | head -n 1)
  ! grep -rnwF -e "${current}" -e 7.0.0 "${repo}/packaging/deb" "${repo}/packaging/rpm" "${repo}/scripts/package/container" \
    "${repo}/scripts/package/build.sh" "${repo}/scripts/package/test.sh" || fail 'a literal release version in a recipe or script'
}

test_layout_comes_from_packaging_arch() {
  local file
  for file in $(sed -n 's/^layout_files=(\(.*\))/\1/p' "${here}/container/package.sh"); do
    [[ -f ${repo}/packaging/arch/${file} ]] || fail "packaging/arch/${file} does not exist"
    grep -q "${file}" "${repo}/packaging/rpm/oxiroute.spec" "${repo}/packaging/deb/debian/rules" || fail "${file} is in no recipe"
  done
  [[ $(grep -c '%config(noreplace)' "${repo}/packaging/rpm/oxiroute.spec") == 3 ]] || fail 'the rpm does not mark the 3 configuration files'
  ! grep -rn 'enable\|restart' "${repo}/packaging/deb/debian/rules" | grep -v 'no-enable' || fail 'the deb enables a service'
}

test_complete_release_is_accepted_and_summed
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
test_recipes_carry_no_version
test_layout_comes_from_packaging_arch
printf 'package selftest passed\n'
