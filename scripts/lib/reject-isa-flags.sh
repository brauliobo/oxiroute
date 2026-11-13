#!/usr/bin/env bash
# Release builds must stay generic x86-64: the binary picks SSE4.2, AVX2 or AVX-512 at run time (crate oxiroute-cpu).
# A baked-in CPU target (a packager's RUSTFLAGS=-Ctarget-cpu=native, CFLAGS=-march=native that the cc crate inherits for
# OpenSSL, aws-lc and the Lua build) makes older CPUs die with SIGILL. reject_isa_flags fails the build instead of
# stripping the flags: any rustc target-cpu or target-feature, and any C flag other than -march=x86-64, -march=x86-64-v1
# and -mtune=generic (what makepkg exports).
#
# Sourced: `source scripts/lib/reject-isa-flags.sh; reject_isa_flags`. Executed: it runs the check itself.
# packaging/arch/PKGBUILD carries an inline copy (makepkg builds a release archive that predates this file); keep them
# equal, scripts/test-verify-baseline-isa.sh runs the same cases against both.
# Not inspected: `[build] rustflags` of cargo config files; scripts/verify-baseline-isa.sh catches those in the binary.

isa_rust_re='^((-C|--codegen)=?)?target-(cpu|feature)'
isa_c_re='^-(march=|mtune=|m(avx|sse3|ssse3|sse4|bmi|fma|popcnt|lzcnt|movbe|f16c|aes|pclmul|sha|vaes|vpclmulqdq|gfni|adx|xop|tbm|evex512))'
isa_ok_re='^-(march=x86-64(-v1)?|mtune=generic)$'

reject_isa_flags() {
  local name flag re tokens
  for name in $(compgen -v); do
    case ${name} in
      RUSTFLAGS | CARGO_ENCODED_RUSTFLAGS | CARGO_BUILD_RUSTFLAGS | CARGO_TARGET_*_RUSTFLAGS) re=${isa_rust_re} ;;
      CC | CXX | CFLAGS | CXXFLAGS | CPPFLAGS | ASFLAGS | HOST_C*FLAGS | TARGET_C*FLAGS | CC_* | CXX_* | CFLAGS_* | CXXFLAGS_*)
        re=${isa_c_re} ;;
      *) continue ;;
    esac
    read -ra tokens <<<"${!name//$'\x1f'/ }"
    for flag in "${tokens[@]}"; do
      [[ ${flag} =~ ${re} && ! ${flag} =~ ${isa_ok_re} ]] || continue
      printf 'build refused: %s carries %s; release binaries stay generic x86-64 and pick AVX at run time (docs/developer/RELEASING.md)\n' \
        "${name}" "${flag}" >&2
      return 1
    done
  done
}

[[ ${BASH_SOURCE[0]} != "$0" ]] || reject_isa_flags
