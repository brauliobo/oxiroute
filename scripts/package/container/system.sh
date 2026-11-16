#!/usr/bin/env bash
# Sourced inside a container: the package manager of the image, whichever family it belongs to.

. /etc/os-release
if command -v apt-get > /dev/null; then pm=apt; else pm=dnf; fi

apt_network=(-o Acquire::Retries=5 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30)

refresh_index() {
  [[ ${pm} == dnf ]] || apt-get update -qq "${apt_network[@]}"
}

install_documentation() {
  if [[ ${pm} == apt ]]; then rm -f /etc/dpkg/dpkg.cfg.d/excludes; else sed -i '/^tsflags=nodocs/d' /etc/dnf/dnf.conf; fi
}

install_packages() {
  if [[ ${pm} == apt ]]; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends "${apt_network[@]}" \
      -o Dpkg::Use-Pty=0 -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold "$@"
  else
    dnf -y -q install --setopt=retries=10 --setopt=timeout=60 "$@"
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
  if [[ ${pm} == apt ]]; then install_packages --reinstall "$1"; else dnf -y -q reinstall --setopt=retries=10 "$1"; fi
}

upgrade_package() {
  if [[ ${pm} == apt ]]; then install_packages "$1"; else dnf -y -q upgrade --setopt=retries=10 "$1"; fi
}
