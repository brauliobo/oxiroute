#!/usr/bin/env bash
# Runs inside a fresh container of an image of the family (scripts/package/test.sh): installs the package, checks what
# it installed and that the program works, then upgrades, removes and purges it.
#
# Environment: FAMILY, ARCH, RELEASE_VERSION, PACKAGE (file name in /out), PREVIOUS (optional file name in /previous: an older
# release package that is installed first and upgraded; its own directory because it can have the name of PACKAGE),
# REQUIRE_PGO (1: the build record must say profile-guided).
# Mounts: /out (the package), /previous (the older package), /package (scripts/package), /packaging (the packaging directory of the source).
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "${here}/system.sh"
source "${here}/../families.sh"

layout=/packaging/arch
smoke=/tmp/smoke
config_files=(oxiroute.env oxiroute.kdl oxiroute.lua)
shared_files=(
  /usr/lib/systemd/system/oxiroute.service:oxiroute.service /usr/lib/sysusers.d/oxiroute.conf:oxiroute.sysusers
  /usr/lib/tmpfiles.d/oxiroute.conf:oxiroute.tmpfiles /usr/lib/modules-load.d/oxiroute.conf:oxiroute.modules
  /usr/lib/oxiroute/ensure-management-token:oxiroute.ensure-token
  /etc/oxiroute/oxiroute.env:oxiroute.env /etc/oxiroute/oxiroute.kdl:oxiroute.kdl /etc/oxiroute/oxiroute.lua:oxiroute.lua
)
pids=()

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf 'ok: %s\n' "$*"; }
absent() { [[ ! -e $1 ]] || fail "$1 exists"; }

stop_daemons() {
  local pid
  for pid in "${pids[@]}"; do kill -TERM "${pid}" 2> /dev/null || true; done
  for pid in "${pids[@]}"; do wait "${pid}" || true; done
  pids=()
}
trap stop_daemons EXIT

prepare_image() {
  install_documentation
  refresh_index
  install_test_tools
  mkdir -p /tmp/install /tmp/previous
  cp -- "/out/${PACKAGE}" /tmp/install/
  [[ -z ${PREVIOUS:-} ]] || cp -- "/previous/${PREVIOUS}" /tmp/previous/
  chmod -R a+rX /tmp/install /tmp/previous
}

install_new() { install_package_file "/tmp/install/${PACKAGE}"; }

installed_version() {
  case ${pm} in
    apt) dpkg-query -W -f='${Version}' oxiroute ;;
    dnf) rpm -q --qf '%{VERSION}' oxiroute ;;
    pacman) pacman -Q oxiroute | cut -d' ' -f2 | sed 's/-1$//' ;;
  esac
}

restore_kdl_migrated_by_install_hook() {
  [[ ${pm} != pacman ]] || cp -- "${layout}/oxiroute.kdl" /etc/oxiroute/oxiroute.kdl
}

install_initial() {
  if [[ -z ${PREVIOUS:-} ]]; then install_new; return; fi
  install_package_file "/tmp/previous/${PREVIOUS}"
  printf '# edited before the upgrade\n' >> /etc/oxiroute/oxiroute.env
  upgrade_package "/tmp/install/${PACKAGE}"
  cmp -s /usr/share/doc/oxiroute/BUILD-INFO.json "/out/${PACKAGE}.build.json" || fail "the upgrade did not install ${PACKAGE}"
  grep -qx '# edited before the upgrade' /etc/oxiroute/oxiroute.env || fail 'the upgrade overwrote a modified configuration'
  sed -i '/^# edited before the upgrade$/d' /etc/oxiroute/oxiroute.env
  restore_kdl_migrated_by_install_hook
  pass "upgraded from ${PREVIOUS}, the modified configuration is kept"
}

check_files() {
  local entry guide
  case ${pm} in pacman) guide='arch-packaging.md' ;; *) guide='INSTALL.md' ;; esac
  for entry in "${shared_files[@]}"; do
    cmp -- "${entry%%:*}" "${layout}/${entry#*:}" || fail "${entry%%:*} differs from packaging/arch/${entry#*:}"
  done
  [[ $(readlink /usr/bin/oxr) == oxiroute ]] || fail '/usr/bin/oxr is not an alias of oxiroute'
  [[ -x /usr/bin/oxiroute && -x /usr/lib/oxiroute/oxiroute-worker-launcher && -x /usr/lib/oxiroute/ensure-management-token ]] || fail 'a binary or helper is missing'
  local asset
  for asset in /usr/share/bash-completion/completions/{oxiroute,oxr} /usr/share/zsh/site-functions/_{oxiroute,oxr} \
    /usr/share/fish/vendor_completions.d/{oxiroute,oxr}.fish; do
    [[ -s ${asset} ]] || fail "${asset} is missing or empty"
  done
  grep -q '^_oxr()' /usr/share/bash-completion/completions/oxr || fail 'the oxr completion does not complete oxr'
  for asset in oxiroute oxr; do
    compgen -G "/usr/share/man/man1/${asset}.1*" > /dev/null || fail "the ${asset} manual page is missing"
  done
  compgen -G "/usr/share/doc/oxiroute/${guide}*" > /dev/null && [[ -f /usr/share/doc/oxiroute/BUILD-INFO.json ]] ||
    fail 'documentation is missing'
  pass 'the files of the layout are installed and equal the ones of packaging/arch'
}

marked_config_files() {
  case ${pm} in
    apt) dpkg-query -W -f='${Conffiles}\n' oxiroute | awk 'NF { print $1 }' ;;
    dnf) rpm -q --qf '[%{FILEFLAGS:fflags}|%{FILENAMES}\n]' oxiroute | awk -F'|' '$1 ~ /c/ && $1 ~ /n/ { print $2 }' ;;
    pacman) pacman -Qii oxiroute | grep -oE '/etc/[^ ]+' ;;
  esac | sort
}

check_config_marking() {
  local expected
  expected=$(printf '/etc/oxiroute/%s\n' "${config_files[@]}" | sort)
  [[ $(marked_config_files) == "${expected}" ]] || fail "the configuration files are not all marked as config: $(marked_config_files)"
  pass 'oxiroute.env, oxiroute.kdl and oxiroute.lua are marked as configuration (never overwritten)'
}

check_account_and_state() {
  local shell mode
  shell=$(getent passwd oxiroute | cut -d: -f7)
  [[ ${shell##*/} == nologin ]] || fail "the oxiroute user has the login shell ${shell}"
  getent group oxiroute > /dev/null || fail 'no oxiroute group'
  for mode in /var/lib/oxiroute:750 /var/lib/oxiroute/audit:700 /var/lib/oxiroute/recordings:750; do
    [[ $(stat -c '%a %U' "${mode%:*}") == "${mode#*:} oxiroute" ]] || fail "${mode%:*} is not ${mode#*:} oxiroute"
  done
  pass 'the oxiroute user has a nologin shell and owns its state directories'
}

check_management_token() {
  local token=/etc/oxiroute/management.token
  [[ $(stat -c '%a %U %G' "${token}") == '600 oxiroute oxiroute' ]] || fail "${token} is not 0600 oxiroute:oxiroute"
  [[ $(wc -c < "${token}") == 64 ]] || fail "${token} does not hold a 64-character token"
  setpriv --reuid oxiroute --regid oxiroute --clear-groups cat "${token}" > /dev/null || fail "the oxiroute user cannot read ${token}"
  pass 'the management token is created, owned by oxiroute and readable only by it'
}

check_nothing_enabled() {
  absent /etc/systemd/system/multi-user.target.wants/oxiroute.service
  ! pgrep -x oxiroute > /dev/null || fail 'the package started the service'
  pass 'the service is neither enabled nor started'
}

check_build_record() {
  local record=/usr/share/doc/oxiroute/BUILD-INFO.json
  cmp -- "${record}" "/out/${PACKAGE}.build.json" || fail 'the packaged build record is not the published one'
  grep -q "\"version\": \"${RELEASE_VERSION}\"" "${record}" || fail "the build record is not for ${RELEASE_VERSION}"
  grep -q "\"target\": \"$(package_target "${ARCH}")\"" "${record}" || fail "the build record is not for ${ARCH}"
  [[ ${REQUIRE_PGO:-1} == 0 ]] || grep -q '"pgo": true' "${record}" || fail 'the packaged binary is not profile-guided'
  pass "the build record: $(tr -d '\n ' < "${record}" | cut -c1-200)"
}

check_commands() {
  [[ $(oxiroute version) == "${RELEASE_VERSION}" && $(oxr version) == "${RELEASE_VERSION}" ]] || fail "version is not ${RELEASE_VERSION}"
  [[ $(installed_version) == "$(package_version "${FAMILY}" "${RELEASE_VERSION}")" ]] || fail 'the package version is not the release version'
  setpriv --reuid oxiroute --regid oxiroute --clear-groups oxiroute config check /etc/oxiroute/oxiroute.kdl \
    || fail 'the shipped default configuration does not check'
  pass "oxiroute ${RELEASE_VERSION} checks the shipped default configuration as the oxiroute user"
}

check_unit() {
  systemd-analyze verify /usr/lib/systemd/system/oxiroute.service || fail 'systemd-analyze verify rejects the unit'
  pass "systemd-analyze verify accepts the unit ($(systemctl --version | head -1))"
}

serve() {
  local log=$1 config=$2 environment
  mapfile -t environment < <(grep -v '^#' /etc/oxiroute/oxiroute.env)
  setpriv --reuid oxiroute --regid oxiroute --clear-groups env "${environment[@]}" /usr/bin/oxiroute serve "${config}" \
    > "${log}" 2>&1 &
  pids+=($!)
}

get() {
  exec 3<> "/dev/tcp/127.0.0.1/$1"
  printf 'GET %s HTTP/1.1\r\nHost: smoke\r\nConnection: close\r\n\r\n' "$2" >&3
  timeout 10 cat <&3
  exec 3>&-
}

# The response of GET PATH on PORT once the daemon of LOG answers with an HTTP status other than 503 (not ready yet).
get_when_ready() {
  local port=$1 path=$2 log=$3 attempt response
  for attempt in $(seq 150); do
    kill -0 "${pids[-1]}" 2> /dev/null || { cat "${log}" >&2; fail "the daemon exited before it answered on ${port}"; }
    if response=$(get "${port}" "${path}" 2> /dev/null) && [[ ${response} == HTTP/1.1\ * && ${response} != HTTP/1.1\ 503* ]]; then
      printf '%s\n' "${response}"
      return
    fi
    sleep 0.2
  done
  cat "${log}" >&2
  fail "nothing ready on ${port}"
}

check_default_service() {
  local response
  serve /tmp/default.log /etc/oxiroute/oxiroute.kdl
  response=$(get_when_ready 8404 /ready /tmp/default.log)
  [[ ${response} == HTTP/1.1\ 200* ]] || { printf '%s\n' "${response}" >&2; fail 'the shipped configuration is not ready'; }
  stop_daemons
  pass 'the shipped default configuration starts and answers /ready'
}

check_proxy() {
  local response
  mkdir -p "${smoke}"
  cp -- /packaging/smoke/origin.kdl /packaging/smoke/proxy.kdl "${smoke}/"
  chmod -R a+rX "${smoke}"
  serve /tmp/origin.log "${smoke}/origin.kdl"
  get_when_ready 18081 / /tmp/origin.log > /dev/null
  serve /tmp/proxy.log "${smoke}/proxy.kdl"
  response=$(get_when_ready 18080 / /tmp/proxy.log)
  [[ ${response} == HTTP/1.1\ 200* && ${response} == *origin-ok* ]] || { printf '%s\n' "${response}" >&2; fail 'the proxy did not relay the origin response'; }
  stop_daemons
  pass 'one HTTP/1 request is relayed through the proxy to a local origin'
}

check_reinstall_keeps_config() {
  local token_before
  token_before=$(cat /etc/oxiroute/management.token)
  printf '# edited after the install\n' >> /etc/oxiroute/oxiroute.env
  reinstall_package "/tmp/install/${PACKAGE}"
  grep -qx '# edited after the install' /etc/oxiroute/oxiroute.env || fail 'a reinstall overwrote a modified configuration'
  [[ $(cat /etc/oxiroute/management.token) == "${token_before}" ]] || fail 'a reinstall replaced the management token'
  check_nothing_enabled
  pass 'a reinstall keeps the modified configuration and management token'
}

check_remove() {
  remove_packages oxiroute
  absent /usr/bin/oxiroute
  absent /usr/lib/systemd/system/oxiroute.service
  [[ -d /var/lib/oxiroute ]] || fail 'remove deleted the state'
  getent passwd oxiroute > /dev/null || fail 'remove deleted the oxiroute user'
  pass 'remove deletes the program and keeps the state and the user'
}

check_remove_config_apt() {
  grep -qx '# edited after the install' /etc/oxiroute/oxiroute.env && [[ -f /etc/oxiroute/oxiroute.kdl ]] \
    || fail 'remove deleted the configuration'
  pass 'remove keeps the configuration'
}

check_remove_config_pacman() {
  grep -qx '# edited after the install' /etc/oxiroute/oxiroute.env.pacsave || fail 'remove did not keep the modified configuration'
  pass 'remove keeps the modified configuration as oxiroute.env.pacsave'
}

check_remove_config_dnf() {
  grep -qx '# edited after the install' /etc/oxiroute/oxiroute.env.rpmsave || fail 'remove did not keep the modified configuration'
  absent /etc/oxiroute/management.token
  pass 'remove keeps the modified configuration as oxiroute.env.rpmsave and deletes the token'
}

check_purge() {
  purge_packages oxiroute
  absent /etc/oxiroute
  absent /var/lib/oxiroute
  absent /run/oxiroute
  pass 'purge deletes the configuration and the state'
}

prepare_image
install_initial
check_files
check_config_marking
check_account_and_state
check_management_token
check_nothing_enabled
check_build_record
check_commands
check_unit
check_default_service
check_proxy
check_reinstall_keeps_config
check_remove
"check_remove_config_${pm}"
[[ ${pm} != apt ]] || check_purge
printf 'PASS %s on %s\n' "${PACKAGE}" "${PRETTY_NAME}"
