#!/usr/bin/env bash
# Release builds must stay generic: x86-64 (the binary picks SSE4.2, AVX2 or AVX-512 at run time, crate oxiroute-cpu) or
# ARMv8-A (NEON and nothing newer). A baked-in CPU target (a packager's RUSTFLAGS=-Ctarget-cpu=native, CFLAGS=-march=native
# that the cc crate inherits for OpenSSL, aws-lc and the Lua build) makes older CPUs die with SIGILL. reject_isa_flags
# fails the build instead of stripping the flags: any rustc target-cpu or target-feature, and any C flag other than the
# generic ones of the target (x86_64: -march=x86-64, -march=x86-64-v1, -mtune=generic, what makepkg exports; aarch64:
# -march=armv8-a, -mcpu=generic, -mtune=generic).
#
# Sourced: `source scripts/lib/reject-isa-flags.sh; reject_isa_flags [TARGET]`, TARGET defaulting to $PGO_TARGET and then to
# x86_64-unknown-linux-gnu. Executed: it runs the check itself.
# packaging/arch/PKGBUILD carries an inline copy for x86_64 (makepkg builds a release archive that predates this file); keep
# the x86_64 rules equal, scripts/test-verify-baseline-isa.sh runs the same cases against both.
# Not inspected: `[build] rustflags` of cargo config files; scripts/verify-baseline-isa.sh catches those in the binary.

isa_rust_re='^((-C|--codegen)=?)?target-(cpu|feature)'
isa_c_re_x86_64='^-(march=|mtune=|m(avx|sse3|ssse3|sse4|bmi|fma|popcnt|lzcnt|movbe|f16c|aes|pclmul|sha|vaes|vpclmulqdq|gfni|adx|xop|tbm|evex512))'
isa_ok_re_x86_64='^-(march=x86-64(-v1)?|mtune=generic)$'
isa_c_re_aarch64='^-(march=|mcpu=|mtune=)'
isa_ok_re_aarch64='^-(march=armv8-a|mcpu=generic|mtune=generic)$'

reject_isa_flags() {
  local target=${1:-${PGO_TARGET:-x86_64-unknown-linux-gnu}} arch name flag re tokens
  arch=${target%%-*}
  [[ ${arch} == x86_64 || ${arch} == aarch64 ]] || { printf 'build refused: no generic CPU baseline is defined for %s\n' "${target}" >&2; return 1; }
  local c_re=isa_c_re_${arch} ok_re=isa_ok_re_${arch}
  for name in $(compgen -v); do
    case ${name} in
      RUSTFLAGS | CARGO_ENCODED_RUSTFLAGS | CARGO_BUILD_RUSTFLAGS | CARGO_TARGET_*_RUSTFLAGS) re=${isa_rust_re} ;;
      CC | CXX | CFLAGS | CXXFLAGS | CPPFLAGS | ASFLAGS | HOST_C*FLAGS | TARGET_C*FLAGS | CC_* | CXX_* | CFLAGS_* | CXXFLAGS_*)
        re=${!c_re} ;;
      *) continue ;;
    esac
    read -ra tokens <<<"${!name//$'\x1f'/ }"
    for flag in "${tokens[@]}"; do
      [[ ${flag} =~ ${re} && ! ${flag} =~ ${!ok_re} ]] || continue
      printf 'build refused: %s carries %s; release binaries stay generic %s and pick their CPU features at run time (docs/developer/RELEASING.md)\n' \
        "${name}" "${flag}" "${arch}" >&2
      return 1
    done
  done
}

[[ ${BASH_SOURCE[0]} != "$0" ]] || reject_isa_flags "$@"
