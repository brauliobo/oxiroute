#!/usr/bin/env bash
# Sourced: runs throwaway containers of the image and architecture of a package.
#
# Environment: PACKAGE_RUNTIME (docker or podman; default: the first installed), PACKAGE_CPUSET (host CPUs the
# container may use, for example 0-7), PACKAGE_NICE (niceness of the container command, default 0), PACKAGE_RUNTIME_ARGS
# (more options of the run command, for example --network host).
# Containers run without the default seccomp profile: it blocks io_uring, the default data plane, and a build that
# trained the epoll plane instead would ship a profile that misses the code every listener runs.

package_scripts=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
package_repo=$(cd -- "${package_scripts}/../.." && pwd)

container_runtime() {
  local runtime
  for runtime in "${PACKAGE_RUNTIME:-}" docker podman; do
    [[ -n ${runtime} ]] && command -v "${runtime}" > /dev/null && { echo "${runtime}"; return; }
  done
  echo 'no container runtime: install docker or podman' >&2
  return 1
}

# container_run ARCH IMAGE [OPTION...] -- COMMAND...
container_run() {
  local arch=$1 image=$2 options=()
  shift 2
  while [[ $1 != -- ]]; do options+=("$1"); shift; done
  shift
  "$(container_runtime)" run --rm --platform "$(container_platform "${arch}")" --security-opt seccomp=unconfined \
    ${PACKAGE_CPUSET:+--cpuset-cpus "${PACKAGE_CPUSET}"} ${PACKAGE_RUNTIME_ARGS:-} "${options[@]}" "${image}" nice -n "${PACKAGE_NICE:-0}" "$@"
}

# The tracked and the untracked, not ignored, files of the working tree as one tar file: the source that is packaged.
source_snapshot() {
  local tar_file=$1 deleted
  export LC_ALL=C
  deleted=$(mktemp)
  git -C "${package_repo}" ls-files -z --deleted > "${deleted}"
  git -C "${package_repo}" ls-files -z --cached --others --exclude-standard | sort -z |
    comm -z -23 - <(sort -z "${deleted}") |
    tar --directory "${package_repo}" --null --files-from - --create --file "${tar_file}"
  rm -f -- "${deleted}"
}
