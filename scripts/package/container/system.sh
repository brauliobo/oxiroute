#!/usr/bin/env bash
# Sourced inside a container: the package manager of the image, whichever family it belongs to.

. /etc/os-release
if command -v apt-get > /dev/null; then pm=apt; elif command -v dnf > /dev/null; then pm=dnf; else pm=pacman; fi

apt_network=(-o Acquire::Retries=5 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30)

dnf_network=(--setopt=retries=10 --setopt=timeout=60)

pacman_install=(--noconfirm --noprogressbar)

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
  case ${pm} in
    apt) apt-get update -qq "${apt_network[@]}" ;;
    dnf) refresh_dnf_metadata ;;
    pacman) pacman -Syu "${pacman_install[@]}" ;;
  esac
}

install_documentation() {
  case ${pm} in
    apt) rm -f /etc/dpkg/dpkg.cfg.d/excludes ;;
    dnf) sed -i '/^tsflags=nodocs/d' /etc/dnf/dnf.conf ;;
    pacman) sed -i '/^NoExtract/d' /etc/pacman.conf ;;
  esac
}

install_packages() {
  case ${pm} in
    apt)
      DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends "${apt_network[@]}" \
        -o Dpkg::Use-Pty=0 -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold "$@" ;;
    dnf) dnf -y -q install "${dnf_network[@]}" "$@" ;;
    pacman) pacman -S --needed "${pacman_install[@]}" "$@" ;;
  esac
}

install_package_file() {
  if [[ ${pm} == pacman ]]; then pacman -U "${pacman_install[@]}" "$1"; else install_packages "$1"; fi
}

install_test_tools() {
  [[ ${pm} == apt ]] || install_packages diffutils procps-ng util-linux
}

remove_packages() {
  case ${pm} in
    apt) apt-get remove -y -qq "$@" ;;
    dnf) dnf -y -q remove "$@" ;;
    pacman) pacman -R "${pacman_install[@]}" "$@" ;;
  esac
}

purge_packages() {
  if [[ ${pm} == apt ]]; then apt-get purge -y -qq "$@"; else dnf -y -q remove "$@"; fi
}

reinstall_package() {
  case ${pm} in
    apt) install_packages --reinstall "$1" ;;
    dnf) dnf -y -q reinstall "${dnf_network[@]}" "$1" ;;
    pacman) install_package_file "$1" ;;
  esac
}

dnf_upgrade_verb() {
  local file_evr installed_evr
  file_evr=$(rpm -qp --qf '%{VERSION}-%{RELEASE}' "$1")
  installed_evr=$(rpm -q --qf '%{VERSION}-%{RELEASE}' oxiroute)
  if [[ ${file_evr} == "${installed_evr}" ]]; then echo reinstall; else echo upgrade; fi
}

upgrade_package() {
  case ${pm} in
    apt) install_packages --reinstall "$1" ;;
    dnf) dnf -y -q "$(dnf_upgrade_verb "$1")" "${dnf_network[@]}" "$1" ;;
    pacman) install_package_file "$1" ;;
  esac
}
