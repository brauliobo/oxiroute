#!/usr/bin/env bash
# Installs the staged release payload and shared layout files into a package build root.
#
#   install-staged.sh STAGE DESTROOT [--no-systemd]
#
# Shared by the deb, rpm and release pacman recipes so all three install the exact same binary,
# symlink, completions, manual pages, launcher, token helper, modules-load config and /etc/oxiroute
# defaults. Pass --no-systemd when the package toolchain installs the unit, sysusers and tmpfiles
# files itself (debhelper).
set -euo pipefail

stage=${1:?stage directory required}
dest=${2:?destination root required}
install_systemd=1
[[ ${3:-} != --no-systemd ]] || install_systemd=0

install -Dm755 "${stage}/payload/bin/oxiroute" "${dest}/usr/bin/oxiroute"
ln -s oxiroute "${dest}/usr/bin/oxr"
install -Dm644 -t "${dest}/usr/share/bash-completion/completions" "${stage}"/payload/share/bash-completion/completions/*
install -Dm644 -t "${dest}/usr/share/zsh/site-functions" "${stage}"/payload/share/zsh/site-functions/*
install -Dm644 -t "${dest}/usr/share/fish/vendor_completions.d" "${stage}"/payload/share/fish/vendor_completions.d/*
install -Dm644 -t "${dest}/usr/share/man/man1" "${stage}"/payload/share/man/man1/*
install -Dm755 "${stage}/payload/lib/oxiroute/oxiroute-worker-launcher" "${dest}/usr/lib/oxiroute/oxiroute-worker-launcher"
install -Dm755 "${stage}/layout/oxiroute.ensure-token" "${dest}/usr/lib/oxiroute/ensure-management-token"
install -Dm644 "${stage}/layout/oxiroute.modules" "${dest}/usr/lib/modules-load.d/oxiroute.conf"
install -Dm644 -t "${dest}/etc/oxiroute" \
  "${stage}/layout/oxiroute.env" "${stage}/layout/oxiroute.kdl" "${stage}/layout/oxiroute.lua"

if (( install_systemd )); then
  install -Dm644 "${stage}/layout/oxiroute.service" "${dest}/usr/lib/systemd/system/oxiroute.service"
  install -Dm644 "${stage}/layout/oxiroute.sysusers" "${dest}/usr/lib/sysusers.d/oxiroute.conf"
  install -Dm644 "${stage}/layout/oxiroute.tmpfiles" "${dest}/usr/lib/tmpfiles.d/oxiroute.conf"
fi
