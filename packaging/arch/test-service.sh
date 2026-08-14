#!/usr/bin/bash
set -euo pipefail

package_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
unit=${1:-"${package_dir}/oxiroute.service"}
pkgbuild=${package_dir}/PKGBUILD
srcinfo=${package_dir}/.SRCINFO
sysusers=${package_dir}/oxiroute.sysusers
tmpfiles=${package_dir}/oxiroute.tmpfiles
environment_file=${package_dir}/oxiroute.env

[[ -f "${unit}" ]] || {
  printf 'service unit not found: %s\n' "${unit}" >&2
  exit 1
}
install_script=${package_dir}/oxiroute.install
[[ -f "${install_script}" ]] || {
  printf 'package install script not found: %s\n' "${install_script}" >&2
  exit 1
}
bash -n "${install_script}"
for path in "${pkgbuild}" "${srcinfo}" "${sysusers}" "${tmpfiles}" "${environment_file}"; do
  [[ -f "${path}" ]] || {
    printf 'package metadata file not found: %s\n' "${path}" >&2
    exit 1
  }
done
if command -v makepkg >/dev/null 2>&1; then
  makepkg --printsrcinfo --dir "${package_dir}" | diff -u "${srcinfo}" -
else
  printf 'skipped makepkg .SRCINFO synchronization check\n'
fi
command -v systemd-analyze >/dev/null 2>&1 || {
  printf 'systemd-analyze is required to validate %s\n' "${unit}" >&2
  exit 1
}
command -v systemd-sysusers >/dev/null 2>&1 || {
  printf 'systemd-sysusers is required to validate %s\n' "${sysusers}" >&2
  exit 1
}
command -v systemd-tmpfiles >/dev/null 2>&1 || {
  printf 'systemd-tmpfiles is required to validate %s\n' "${tmpfiles}" >&2
  exit 1
}
systemd_version=$(systemd-analyze --version)
systemd_version=${systemd_version#systemd }
systemd_version=${systemd_version%% *}
if (( systemd_version >= 257 )); then
  systemd-analyze verify "${unit}"
else
  printf 'skipped systemd-analyze verification: systemd 257 is required for ProtectControlGroups=private\n'
fi

declare -A environment_values=()
while IFS= read -r line; do
  case "${line}" in
    ''|'#'*) continue ;;
  esac
  [[ "${line}" == *=* ]] || {
    printf 'invalid environment entry: %s\n' "${line}" >&2
    exit 1
  }
  environment_values["${line%%=*}"]=${line#*=}
done <"${environment_file}"

[[ "${environment_values[OXIROUTE_AUDIT_DIR]:-}" == '/var/lib/oxiroute/audit' ]] || {
  printf 'package environment does not configure the durable audit directory\n' >&2
  exit 1
}
for expected in \
  'OXIROUTE_AUDIT_MAX_RECORDS=10000' \
  'OXIROUTE_AUDIT_MAX_RECORD_BYTES=16384' \
  'OXIROUTE_AUDIT_MAX_FILE_BYTES=1048576' \
  'OXIROUTE_AUDIT_MAX_TOTAL_BYTES=8388608' \
  'OXIROUTE_AUDIT_MAX_ROTATED_FILES=7'; do
  [[ "${environment_values[${expected%%=*}]:-}" == "${expected#*=}" ]] || {
    printf 'package environment is missing bounded audit setting: %s\n' "${expected}" >&2
    exit 1
  }
done

declare -A service_entries=()
while IFS= read -r line; do
  case "${line}" in
    ConditionControlGroupController=*|EnvironmentFile=*|StateDirectory=*|StateDirectoryMode=*|ReadWritePaths=*|User=*|Group=*|NoNewPrivileges=*|CapabilityBoundingSet=*|AmbientCapabilities=*|Delegate=*|DelegateSubgroup=*|ProtectControlGroups=*)
      service_entries["${line%%=*}"]=${line#*=}
      ;;
  esac
done <"${unit}"
[[ "${service_entries[EnvironmentFile]:-}" == '-/etc/oxiroute/oxiroute.env' ]] || {
  printf 'service unit does not load the package environment file\n' >&2
  exit 1
}
[[ "${service_entries[StateDirectory]:-}" == 'oxiroute' ]] || {
  printf 'service unit does not declare the persistent state directory\n' >&2
  exit 1
}
[[ "${service_entries[StateDirectoryMode]:-}" == '0750' ]] || {
  printf 'service unit has an unexpected state directory mode\n' >&2
  exit 1
}
[[ "${service_entries[ReadWritePaths]:-}" == '/run/oxiroute /var/lib/oxiroute' ]] || {
  printf 'service unit has an unexpected writable path set\n' >&2
  exit 1
}
[[ "${service_entries[User]:-}" == 'oxiroute' && "${service_entries[Group]:-}" == 'oxiroute' ]] || {
  printf 'service unit does not retain the unprivileged oxiroute identity\n' >&2
  exit 1
}
[[ "${service_entries[NoNewPrivileges]:-}" == 'true' ]] || {
  printf 'service unit dropped NoNewPrivileges=true\n' >&2
  exit 1
}
[[ "${service_entries[ConditionControlGroupController]:-}" == 'v2' ]] || {
  printf 'service unit does not require a unified cgroup v2 hierarchy\n' >&2
  exit 1
}
[[ -v 'service_entries[Delegate]' && -z "${service_entries[Delegate]}" ]] || {
  printf 'service unit must delegate the cgroup subtree without requesting controllers\n' >&2
  exit 1
}
[[ "${service_entries[DelegateSubgroup]:-}" == 'supervisor' ]] || {
  printf 'service unit does not pin the main process to the supervisor cgroup\n' >&2
  exit 1
}
[[ "${service_entries[ProtectControlGroups]:-}" == 'private' ]] || {
  printf 'service unit does not isolate the delegated cgroup subtree\n' >&2
  exit 1
}
[[ "${service_entries[CapabilityBoundingSet]:-}" == 'CAP_NET_BIND_SERVICE' &&
   "${service_entries[AmbientCapabilities]:-}" == 'CAP_NET_BIND_SERVICE' ]] || {
  printf 'service unit changed the bounded bind capability\n' >&2
  exit 1
}

supervisor_package=
launcher_install=
while IFS= read -r line; do
  case "${line}" in
    *'--package oxiroute-supervisor-process'*) supervisor_package=${line} ;;
    */usr/lib/oxiroute/oxiroute-worker-launcher*) launcher_install=${line} ;;
  esac
done <"${pkgbuild}"
[[ -n "${supervisor_package}" ]] || {
  printf 'package build does not compile the worker supervision crate\n' >&2
  exit 1
}
[[ -n "${launcher_install}" ]] || {
  printf 'package does not install the supervised worker launcher\n' >&2
  exit 1
}

lifecycle_root=$(mktemp -d)
trap 'rm -rf -- "${lifecycle_root}"' EXIT
systemd-sysusers --root="${lifecycle_root}" "${sysusers}"
if command -v fakeroot >/dev/null 2>&1; then
  fakeroot -- sh -c '
    set -eu
    systemd-tmpfiles --root="$1" --create "$2"
    test "$(stat -c %a "$1/var/lib/oxiroute")" = 750
    test "$(stat -c %a "$1/var/lib/oxiroute/audit")" = 700
    test "$(stat -c %a "$1/var/lib/oxiroute/recordings")" = 750
  ' sh "${lifecycle_root}" "${tmpfiles}"
else
  systemd-tmpfiles --root="${lifecycle_root}" --dry-run --create "${tmpfiles}"
  printf 'skipped fakeroot package ownership lifecycle check\n'
fi

exec_start=
exec_reload=
while IFS= read -r line; do
  case "${line}" in
    ExecStart=*) exec_start=${line} ;;
    ExecReload=*) exec_reload=${line} ;;
  esac
done <"${unit}"

[[ "${exec_start}" == 'ExecStart=/usr/bin/oxiroute serve /etc/oxiroute/oxiroute.kdl' ]] || {
  printf 'unexpected ExecStart in %s: %s\n' "${unit}" "${exec_start}" >&2
  exit 1
}
[[ "${exec_reload}" == 'ExecReload=/bin/kill -HUP $MAINPID' ]] || {
  printf 'unexpected ExecReload in %s: %s\n' "${unit}" "${exec_reload}" >&2
  exit 1
}

printf 'verified service reload command: %s\n' "${exec_reload}"
