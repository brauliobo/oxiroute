#!/usr/bin/env bash
# Installs the BOLT post-link optimiser (llvm-bolt, merge-fdata and the instrumentation runtime) into a directory
# without root and prints the directory that holds the tools. The package is downloaded by URL and used only after its
# SHA-256 matches the value pinned here; a mismatch, a missing file or an unreachable mirror fails the script.
#
#   install-bolt.sh DEST          DEST/usr/lib/llvm-18/bin has llvm-bolt, merge-fdata, perf2bolt (lib/ the runtime)
#
# The pin is Ubuntu 24.04 (noble) package bolt-18 18.1.3-1, the version the release runner image ships the LLVM 18
# packages of. Update it together with the version in docs/developer/RELEASING.md (sha256 from the `Packages` index of
# the Ubuntu archive).
set -euo pipefail

version=18.1.3-1
# name:sha256 of each package (bolt-18 holds the tools, libbolt-18-dev the instrumentation runtime library)
packages=(
  bolt-18:9e6fa9be4de3765ffd56ccb49760c9faaa789beab9f23379fd4f7936d62b19f9
  libbolt-18-dev:1bf4ab5664ee661d74c05008fdcbe3549edf4454c98f116519a9a8f7dc7330c6
)
mirrors=(
  "http://archive.ubuntu.com/ubuntu/pool/universe/l/llvm-toolchain-18"
  "https://launchpad.net/ubuntu/+archive/primary/+files"
)

dest=${1:-}
[[ -n ${dest} && ${dest} != -* ]] || { echo "usage: $0 DEST" >&2; exit 2; }
dest=$(realpath -m -- "${dest}")
download=$(mktemp -d)
trap 'rm -rf -- "${download}"' EXIT

# Every mirror serves the same pinned bytes; whichever answers is verified, never trusted.
fetch() {
  local file=$1 sha256=$2 mirror
  for mirror in "${mirrors[@]}"; do
    curl --fail --silent --show-error --location --retry 3 --max-time 300 --output "${download}/${file}" \
      "${mirror}/${file}" || continue
    echo "${sha256}  ${download}/${file}" | sha256sum --check --status && return
    echo "checksum mismatch for ${file} from ${mirror}" >&2
    exit 1
  done
  echo "could not download ${file}" >&2
  exit 1
}

for entry in "${packages[@]}"; do
  file=${entry%%:*}_${version}_amd64.deb
  fetch "${file}" "${entry#*:}"
  mkdir -p -- "${dest}"
  dpkg-deb --extract "${download}/${file}" "${dest}"
done
bin=${dest}/usr/lib/llvm-18/bin
"${bin}/llvm-bolt" --version | head -1 >&2
echo "${bin}"
