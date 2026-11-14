#!/usr/bin/env bash
# Offline tests of scripts/verify-baseline-isa.sh (assembled fixture binaries with and without AVX code, qemu when
# installed) and of the CPU-target flag guard (scripts/lib/reject-isa-flags.sh, its copy in packaging/arch/PKGBUILD,
# and the build scripts that call it).
# shellcheck disable=SC2016 # assembler and `bash -c` text is meant literally
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
verify=${repo}/scripts/verify-baseline-isa.sh
root=$(mktemp -d)
trap 'rm -rf -- "${root}"' EXIT

ok() { printf 'ok: %s\n' "$1"; }
die() { printf 'FAILED: %s\n' "$*" >&2; exit 1; }

# fixture NAME [-s] [--exec] SYMBOL=BODY...: a static binary without libc; _start exits 0 (after calling the first
# symbol when --exec is given), every SYMBOL=BODY is a function holding the instructions of BODY.
fixture() {
  local name=$1 exec=0 strip=() body sym
  shift
  while [[ ${1:-} == -* ]]; do
    case $1 in -s) strip=(-s) ;; --exec) exec=1 ;; esac
    shift
  done
  {
    printf '.text\n.globl _start\n_start:\n'
    (( exec == 0 )) || printf 'call %s\n' "${1%%=*}"
    printf 'mov $60, %%eax\nxor %%edi, %%edi\nsyscall\n'
    for body in "$@"; do
      sym=${body%%=*}
      printf '.type %s,@function\n%s:\n%s\nret\n' "${sym}" "${sym}" "${body#*=}"
    done
  } >"${root}/${name}.s"
  cc -nostdlib -static -Wl,--build-id "${strip[@]}" -o "${root}/${name}" "${root}/${name}.s"
}

accepts() { "${verify}" "$@" >"${root}/out" 2>&1 || { cat "${root}/out" >&2; die "rejected: $*"; }; }
rejects() { # PATTERN verify-arguments...
  local pattern=$1
  shift
  if "${verify}" "$@" >"${root}/out" 2>&1; then die "accepted: $*"; fi
  grep -Eq -- "${pattern}" "${root}/out" || { cat "${root}/out" >&2; die "no '${pattern}' in the output for: $*"; }
}

legacy_memchr=_ZN6memchr4arch6x86_644avx26finder17h0123456789abcdefE
legacy_app=_ZN8oxiroute6server4pump17h0123456789abcdefE
legacy_cpu=_ZN12oxiroute_cpu8dispatch4find17h0123456789abcdefE

fixture clean 'f=popcnt %rax, %rax; tzcnt %rax, %rax; pcmpistri $0, %xmm1, %xmm0; verw (%rax)'
accepts "${root}/clean"
ok 'a binary without AVX passes'

# One function per allowed family (scripts/verify-baseline-isa.sh), each with an instruction of its class.
fixture dispatched "${legacy_memchr}=vpxor %ymm0, %ymm0, %ymm0" "${legacy_cpu}=vzeroupper; shlx %rax, %rbx, %rcx" \
  '_ZN12oxiroute_cpu8dispatch5sse4217h0123456789abcdefE=vpxor %xmm0, %xmm0, %xmm0' \
  'aws_lc_0_1_0_sha256_block_data_order_avx2=vpaddd %ymm0, %ymm1, %ymm2' \
  'Lcurve25519_x25519_scalarloop=mulx %rax, %rbx, %rcx' 'Lp256_montjscalarmul_p256_montjadd=mulx %rax, %rbx, %rcx' \
  'Lbignum_mod_n25519_writeback=adcx %rax, %rbx' 'Lsha3_keccak4_f1600_alt=vmovdqu (%rax), %ymm0' \
  'Lmlk_rej_uniform_asm_loop_start=pext %rax, %rbx, %rcx' 'GFMUL=vpclmulqdq $0, %xmm0, %xmm1, %xmm2' \
  'partial_fold=vpxor %xmm0, %xmm0, %xmm0' \
  'ring_core_0_17_8__gcm_ghash_vpclmulqdq_avx2_1=vpxor %ymm0, %ymm0, %ymm0' \
  'OPENSSL_cleanse_avx=vpxor %xmm0, %xmm0, %xmm0' 'x25519_fe51_mul=mulx %rax, %rbx, %rcx' \
  'adler32_fold_copy_avx2=vpxor %ymm0, %ymm0, %ymm0' 'chunkcopy_safe=vmovdqu (%rax), %ymm0' 'ZSTD_decompress_bmi2=shlx %rax, %rbx, %rcx' \
  'je_malloc_x=andn %rax, %rbx, %rcx' '__strchr_avx2=vpcmpeqb (%rdi), %ymm0, %ymm1' \
  'se_fma=vfmadd231ps %xmm0, %xmm1, %xmm2' 'Lcurve25519_x25519_midloop.cold.0=mulx %rax, %rbx, %rcx'
accepts "${root}/dispatched"
for family in 'aws-lc 9' 'isa-named 2' 'jemalloc-glibc 1' 'openssl 2' 'oxiroute-cpu 2' 'ring 1' 'rust-dispatch 1' 'zlib-ng 2' 'zstd 1'; do
  grep -q "${family% *} ${family#* }" "${root}/out" || { cat "${root}/out" >&2; die "dispatched count of ${family% *}"; }
done
grep -q 'baseline ISA ok (21 dispatched functions: ' "${root}/out" || die 'dispatched function count'
ok 'every allowed family passes and is counted by origin'

for leak in 'vpxor %xmm0, %xmm0, %xmm0' 'mulx %rax, %rbx, %rcx' 'lzcnt %rax, %rbx' 'movbe (%rax), %rbx' \
  'vmovdqu8 (%rax), %zmm0' 'kmovq %k1, %rax'; do
  fixture leak "${legacy_app}=${leak}" "${legacy_memchr}=vpxor %ymm0, %ymm0, %ymm0"
  rejects "oxiroute::server::pump: ${leak%% *}" "${root}/leak"
  grep -q 'memchr' "${root}/out" && die "the dispatched function is listed for ${leak}"
done
ok 'VEX, BMI, LZCNT, MOVBE and AVX-512 in other functions fail and name the function'

body=()
for i in $(seq 25); do body+=("leak${i}=vpxor %xmm0, %xmm0, %xmm0"); done
fixture crowd "${body[@]}"
rejects '25 function' "${root}/crowd"
[[ $(grep -c '^  leak' "${root}/out") == 25 ]] || die 'every offender is listed'
ok 'every offender is listed'

# Names that look like an allowed family but are our own or compiler-generated code are still leaks, grouped by kind.
fixture lookalike 'Lfoo_scalarloop=mulx %rax, %rbx, %rcx' \
  '_ZN6memchrX3foo17h0123456789abcdefE=vpxor %xmm0, %xmm0, %xmm0' \
  "${legacy_app}=vpxor %xmm0, %xmm0, %xmm0" 'oxiroute_cpu_helper=lzcnt %rax, %rbx' 'GFMUL2=vpxor %xmm0, %xmm0, %xmm0'
rejects '5 function' "${root}/lookalike"
for leak in Lfoo_scalarloop 'oxiroute::server::pump' oxiroute_cpu_helper GFMUL2 _ZN6memchrX3foo17h0123456789abcdefE; do
  grep -qF "  ${leak}: " "${root}/out" || { cat "${root}/out" >&2; die "${leak} is not listed"; }
done
grep -q '^Rust code:$' "${root}/out" || die 'no Rust code group'
grep -q '^C or assembly symbols:$' "${root}/out" || die 'no C or assembly group'
ok 'lookalike names and own code fail, grouped by kind'

fixture stripped -s "${legacy_app}=vpxor %xmm0, %xmm0, %xmm0"
rejects 'stripped binary.*--symbols' "${root}/stripped"
ok 'a stripped binary fails closed'

fixture good_unstripped "${legacy_memchr}=vpxor %ymm0, %ymm0, %ymm0"
strip -o "${root}/good_stripped" "${root}/good_unstripped"
accepts --symbols "${root}/good_unstripped" "${root}/good_stripped"
rejects 'build-id' --symbols "${root}/clean" "${root}/good_stripped"
ok '--symbols checks the build-id and disassembles the unstripped file'

printf 'not an elf\n' >"${root}/text"
rejects 'file format not recognized' "${root}/text"
rejects 'usage' --run
ok 'bad input is refused'

qemu=${QEMU_X86_64:-qemu-x86_64}
if command -v "${qemu}" >/dev/null; then
  accepts --run "${root}/clean"
  grep -q 'runs on qemu CPU Nehalem' "${root}/out" || die 'no run on Nehalem'
  grep -q 'runs on qemu CPU Denverton' "${root}/out" || die 'no run on Denverton'
  fixture sigill --exec "${legacy_memchr}=vpxor %ymm0, %ymm0, %ymm0"
  accepts "${root}/sigill"
  rejects 'qemu CPU Nehalem.*132|SIGILL' --run "${root}/sigill"
  ISA_QEMU_CPUS=NoSuchCpu rejects 'does not list the CPU NoSuchCpu' --run "${root}/clean"
  ok '--run passes on qemu CPUs without AVX and fails on SIGILL'
else
  printf 'SKIP: %s is not installed, the qemu cases did not run\n' "${qemu}"
fi
QEMU_X86_64=qemu-x86_64-missing accepts --run "${root}/clean"
grep -q 'runtime check SKIPPED' "${root}/out" || die 'SKIPPED line'
QEMU_X86_64=qemu-x86_64-missing rejects 'required but' --run --require "${root}/clean"
ok 'a missing qemu skips the run check, and fails with --require'

# Flag guard: the library and the inline copy of the PKGBUILD must agree.
guard_lib() { env -i PATH="${PATH}" "$@" bash -c 'set -euo pipefail; source "$1"; reject_isa_flags' _ "${repo}/scripts/lib/reject-isa-flags.sh"; }
guard_pkgbuild() { env -i PATH="${PATH}" "$@" bash -c 'set -euo pipefail; source "$1"; reject_isa_flags' _ "${repo}/packaging/arch/PKGBUILD"; }
guard_accepts() { local guard; for guard in guard_lib guard_pkgbuild; do "${guard}" "$@" 2>&1 || die "${guard} rejected: $*"; done; }
guard_rejects() {
  local guard
  for guard in guard_lib guard_pkgbuild; do
    ! "${guard}" "$@" >/dev/null 2>"${root}/err" || die "${guard} accepted: $*"
    grep -q '^build refused: ' "${root}/err" || die "${guard}: no refusal message for: $*"
  done
}

guard_accepts
guard_accepts RUSTFLAGS='-Cforce-frame-pointers=yes -Cpanic=abort -Clink-arg=-Wl,-q --remap-path-prefix=/a=/b'
guard_accepts CARGO_ENCODED_RUSTFLAGS=$'-Cpanic=abort\x1f-Cforce-frame-pointers=yes'
guard_accepts CFLAGS='-march=x86-64 -mtune=generic -O2 -pipe -fno-plt -Wp,-D_FORTIFY_SOURCE=3' CXXFLAGS='-march=x86-64-v1 -mtune=generic'
guard_accepts CFLAGS_x86_64_unknown_linux_gnu=-O3 CC=cc
guard_rejects RUSTFLAGS=-Ctarget-cpu=native
guard_rejects RUSTFLAGS='-Cpanic=abort -Ctarget-cpu=x86-64'
guard_rejects RUSTFLAGS='-C target-cpu=native'
guard_rejects RUSTFLAGS='-Ctarget-feature=+avx2'
guard_rejects RUSTFLAGS='-C target-feature=-avx'
guard_rejects RUSTFLAGS='--codegen=target-cpu=native'
guard_rejects CARGO_ENCODED_RUSTFLAGS=$'-Cpanic=abort\x1f-Ctarget-cpu=native'
guard_rejects CARGO_ENCODED_RUSTFLAGS=$'-C\x1ftarget-feature=+bmi2'
guard_rejects CARGO_BUILD_RUSTFLAGS=-Ctarget-cpu=native
guard_rejects CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS=-Ctarget-feature=+sse4.2
for flag in -march=native -march=x86-64-v2 -march=x86-64-v3 -march=x86-64-v4 -march=znver4 -mtune=native -mtune=znver3 \
  -mavx -mavx2 -mavx512f -msse4.2 -mbmi2 -mfma -mpopcnt -mlzcnt -mmovbe; do
  guard_rejects CFLAGS="-O2 ${flag}"
  guard_rejects CXXFLAGS="${flag}"
  guard_rejects CFLAGS_x86_64_unknown_linux_gnu="-O3 ${flag}"
  guard_rejects TARGET_CFLAGS="${flag}"
  guard_rejects CC="gcc ${flag}"
done
ok 'the flag guard (library and PKGBUILD copy) rejects CPU targets and accepts generic flags'

# The build scripts refuse before they build anything.
mkdir -p "${root}/bin" "${root}/work"
printf '#!/usr/bin/env bash\necho cargo ran >>"%s/cargo.log"\n' "${root}" >"${root}/bin/cargo"
chmod +x "${root}/bin/cargo"
build_refuses() { # VAR=VALUE command...
  local var=$1
  shift
  ! env PATH="${root}/bin:${PATH}" PGO=0 BOLT=0 PGO_WORK="${root}/work" LLVM_PROFDATA=/bin/true "${var}" "$@" \
    >"${root}/out" 2>&1 || die "$* accepted ${var}"
  grep -q '^build refused: ' "${root}/out" || die "$*: no refusal for ${var}"
}
for var in RUSTFLAGS=-Ctarget-cpu=native CFLAGS=-march=native; do
  build_refuses "${var}" "${repo}/benchmarks/scripts/pgo.sh" plain
  build_refuses "${var}" "${repo}/scripts/build-release-binaries.sh" 9.9.9 "${root}/out-dir"
done
[[ ! -e ${root}/cargo.log ]] || die 'cargo ran after a refusal'
PATH="${root}/bin:${PATH}" PGO_WORK="${root}/work" LLVM_PROFDATA=/bin/true "${repo}/benchmarks/scripts/pgo.sh" plain
grep -q 'cargo ran' "${root}/cargo.log" || die 'pgo.sh did not build with generic flags'
ok 'pgo.sh and build-release-binaries.sh refuse CPU targets before building'

printf 'verify-baseline-isa tests passed\n'
