#!/usr/bin/env bash
# Sourced inside a container: the package manager of the image, whichever family it belongs to.

. /etc/os-release
if command -v apt-get > /dev/null; then pm=apt; else pm=dnf; fi

apt_network=(-o Acquire::Retries=5 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30)

dnf_network=(--setopt=retries=10 --setopt=timeout=60)

refresh_dnf_metadata() {
  local attempt
  for attempt in 1 2 3 4; do
    dnf -y -q makecache "${dnf_network[@]}" && return
    dnf -y -q clean metadata
    sleep $((attempt * 5))
  done
  dnf -y -q makecache "${dnf_network[@]}"
}

refresh_index() {
  if [[ ${pm} == apt ]]; then apt-get update -qq "${apt_network[@]}"; else refresh_dnf_metadata; fi
}

install_documentation() {
  if [[ ${pm} == apt ]]; then rm -f /etc/dpkg/dpkg.cfg.d/excludes; else sed -i '/^tsflags=nodocs/d' /etc/dnf/dnf.conf; fi
}

install_packages() {
  if [[ ${pm} == apt ]]; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends "${apt_network[@]}" \
      -o Dpkg::Use-Pty=0 -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold "$@"
  else
    dnf -y -q install "${dnf_network[@]}" "$@"
  fi
}

install_test_tools() {
  [[ ${pm} == apt ]] || install_packages diffutils procps-ng util-linux
}

remove_packages() {
  if [[ ${pm} == apt ]]; then apt-get remove -y -qq "$@"; else dnf -y -q remove "$@"; fi
}

purge_packages() {
  if [[ ${pm} == apt ]]; then apt-get purge -y -qq "$@"; else dnf -y -q remove "$@"; fi
}

reinstall_package() {
  if [[ ${pm} == apt ]]; then install_packages --reinstall "$1"; else dnf -y -q reinstall "${dnf_network[@]}" "$1"; fi
}

dnf_upgrade_verb() {
  local file_evr installed_evr
  file_evr=$(rpm -qp --qf '%{VERSION}-%{RELEASE}' "$1")
  installed_evr=$(rpm -q --qf '%{VERSION}-%{RELEASE}' oxiroute)
  if [[ ${file_evr} == "${installed_evr}" ]]; then echo reinstall; else echo upgrade; fi
}

upgrade_package() {
  if [[ ${pm} == apt ]]; then
    install_packages --reinstall "$1"
  else
    dnf -y -q "$(dnf_upgrade_verb "$1")" "${dnf_network[@]}" "$1"
  fi
}
