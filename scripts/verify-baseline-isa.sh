#!/usr/bin/env bash
# Fails when a release binary executes instructions that a generic x86-64 CPU (an Intel Celeron J4125: SSE4.2 and POPCNT,
# no AVX) or a generic ARMv8-A CPU (a Cortex-A53: NEON, no extension of ARMv8.1 or later) lacks, outside code that is
# only reached through run-time CPU dispatch. The ELF machine of BIN selects the architecture.
#
#   verify-baseline-isa.sh [--run] [--require] [--symbols UNSTRIPPED] BIN
#
# AArch64: a function containing an instruction of an optional or later extension (LSE atomics, CRC32, the crypto,
# dot-product, FP16, SVE/SME, pointer-authentication, MTE, RCpc, BF16/I8MM and rounding extensions) passes only when its
# symbol matches DISPATCHED_SYMBOL_PATTERNS (the libgcc and compiler-builtins `__aarch64_*` outline-atomics helpers test
# the kernel's hardware capabilities before they run an LSE instruction; hint-space PAC and BTI execute as no-ops on
# older CPUs and are not flagged). The disassembler is GNU `objdump` of an arm64 host or `llvm-objdump` (OBJDUMP
# overrides). --run executes `BIN version` natively on an aarch64 host (the host CPU is then not the baseline) and under
# `qemu-aarch64 -cpu cortex-a53` (ISA_QEMU_CPUS, QEMU_AARCH64) elsewhere.
#
# Static check (always): `objdump -d -C` of BIN is split per function symbol. A function containing a VEX/EVEX
# instruction (every v* mnemonic but verr/verw/vm*, the AVX-512 mask instructions), BMI1/BMI2 (andn bextr blsi blsmsk
# blsr bzhi mulx pdep pext rorx sarx shlx shrx), lzcnt, movbe, adcx or adox passes only when its demangled symbol matches
# DISPATCHED_SYMBOL_PATTERNS below; any other function is listed (first 20, with the first offending mnemonic) and fails.
# FMA is VEX-encoded, so it is covered. tzcnt is not flagged: without BMI1 it decodes as `rep bsf`, which compilers emit
# on purpose for generic x86-64. Non-VEX SSE4.x, POPCNT, AES-NI, PCLMUL and SHA-NI are outside the contract (the J4125
# class has them, and the Rust code checks them at run time anyway).
#
# Stripped binaries: with no .symtab there are no function boundaries, so the check cannot tell dispatched code from a
# leak. It does not guess: it fails closed. Pass --symbols UNSTRIPPED (the same link before `strip`, matched by GNU
# build-id; that file is what gets disassembled) or check the unstripped binary; --run still proves that it starts.
#
# Run check (--run): `BIN version` (the offline subcommand of crates/oxiroute-server, no configuration needed) under
# `qemu-x86_64 -cpu CPU` for each of ISA_QEMU_CPUS (default: Nehalem Denverton). Denverton is the Goldmont-class Atom
# (SSE4.2, no AVX) that stands in for the J4125 (Goldmont Plus), which qemu 11 does not list. A first run under the
# richest CPU (-cpu max) separates an ISA failure from a loader problem; a CPU name that qemu does not list fails.
# qemu-x86_64 (QEMU_X86_64 overrides the name) missing: "SKIPPED" and exit 0, or exit 1 with --require. A dynamic binary
# needs its loader and libraries, found at the host root, or under QEMU_LD_PREFIX for a baseline sysroot; a
# distribution whose glibc requires x86-64-v3 raises SIGILL in ld.so before BIN runs, so use such a sysroot there.
#
# Exit: 0 ok (or run check skipped), 1 violation, 2 usage.
set -euo pipefail
export LC_ALL=C # objdump and readelf translate the headers that are parsed below

# Extend here. Every entry is a family of symbols of known third-party or run-time dispatched code: an ORIGIN and EREs
# matched against the demangled symbol (unanchored unless they start with ^). A family belongs here only when its code
# runs after a CPU feature check; the evidence is the comment of the family. A symbol that names its own ISA (avx, evex,
# bmi, ...) is also accepted. Anything else is our own or compiler-generated code and fails, whatever it executes.
DISPATCHED_SYMBOL_PATTERNS=()
family() {
  local origin=$1 re
  shift
  for re in "$@"; do DISPATCHED_SYMBOL_PATTERNS+=("${origin}"$'\t'"${re}"); done
}
b='(^|[^[:alnum:]_])'
# oxiroute-cpu variants and the consumers' #[target_feature] kernels, chosen by oxiroute_cpu at run time
family oxiroute-cpu "${b}oxiroute_cpu::" "${b}oxiroute_h1_head::.*(sse42|avx)"
# Rust dependencies that detect the CPU at run time
family rust-dispatch "${b}(memchr|aho_corasick|crc32fast|sha1|sha2|chacha20|cpufeatures|simdutf8|bytecount|compiler_builtins)::"
# AArch64: libgcc and compiler-builtins outline atomics call an LSE instruction only after reading the hardware
# capabilities (__aarch64_have_lse_atomics)
family outline-atomics '^_*aarch64_(cas|swp|ldadd|ldclr|ldeor|ldset)[0-9]_'
# aws-lc (aws-lc-sys): exported symbols carry the aws_lc_<version>_ prefix. Its assembly keeps the local labels of its
# routines as symbols without it, and every label belongs to a routine that its C callers reach only after a check:
#  - s2n-bignum (curve25519, edwards25519, p256, p384, p521, secp256k1, sm2, sha3, bignum_*) labels are L<routine>_<label>;
#    use_s2n_bignum_alt() in s2n-bignum_aws-lc.h selects the BMI2/ADX routines only when CRYPTO_is_BMI2_capable() and
#    CRYPTO_is_ADX_capable(); the SHA-3 x4 routine needs CRYPTO_is_AVX2_capable() (crypto/fipsmodule/sha/keccak1600.c)
#  - ML-KEM (mlkem-native) labels Lmlk_*: mlk_sys_check_capability() requires AVX2 and BMI2 (mlkem_native_config.h)
#  - AES-GCM-SIV (GFMUL and the perlasm labels): used only when CRYPTO_is_AVX_capable() and CRYPTO_is_AESNI_capable()
#    (crypto/cipher_extra/e_aesgcmsiv.c); the other perlasm labels are local labels (.L*) or named like the routines
family aws-lc '^_*aws_lc' '^L(bignum|curve25519|edwards25519|p256|p384|p521|secp256k1|sm2|sha3|mlk)_' \
  '^(GFMUL$|partial_fold|iv_len_12_init_IV|skip_iv_len_12_init_IV|hw_x86_64_)' '^[.]'
family ring '^_*ring_core'
# OpenSSL (vendored by openssl-sys) and its perlasm: capability vector OPENSSL_ia32cap_P, tested by the C callers
#  AArch64 perlasm: capability vector OPENSSL_armcap_P (aes_v8_*, gcm_*_v8, sha*_block_armv8, SHA3_*, KeccakF1600)
family openssl '^_*(OPENSSL|ossl|CRYPTO|aesni|aes_hw|aes_v8|aes_gcm|vpaes|bn_|ecp_nistz256|x25519|gcm_|sha[0-9]+_|SHA3_)' \
  '^_*(chacha20|ChaCha20|poly1305|rsaz|md5_|keccak|Keccak|mulx|sqrx)'
# zlib-ng (libz-ng-sys): its per-ISA files (arch/x86/*_avx2.c, *_avx512.c) are chosen by init_functable() only when the
# CPU has the feature (functable.c: has_avx2 && has_bmi2 ...); chunkcopy_safe is the static inline of inflate_p.h that
# the AVX2 and AVX-512 builds of inflate_fast inline
family zlib-ng '^(adler32|crc32|chunkset|compare256|slide_hash|longest_match|inflate_fast|chunkcopy_safe$)'
family zstd '^(ZSTD_|HUF_|FSE_)'
family jemalloc-glibc '^(je_|_rjem|__libc_|_dl_)'
family isa-named '(^|[^[:alpha:]])(avx|evex|bmi|movbe|fma|vaes|vpclmul|vpclmulqdq|mulx|adx|rorx)([^[:alpha:]]|$)'

usage() { echo "usage: $0 [--run] [--require] [--symbols UNSTRIPPED] BIN" >&2; exit 2; }
fail() { printf '%s: %s\n' "${bin}" "$*" >&2; exit 1; }

run=0 require=0 symbols=
while [[ ${1:-} == -* ]]; do
  case $1 in
    --run) run=1 ;;
    --require) require=1 ;;
    --symbols)
      symbols=${2:-}
      [[ -f ${symbols} ]] || usage
      shift ;;
    *) usage ;;
  esac
  shift
done
bin=${1:-}
[[ -f ${bin} && $# == 1 ]] || usage
[[ ${require} == 0 || ${run} == 1 ]] || usage

build_id() { readelf -n "$1" | sed -n 's/.*Build ID: //p'; }

# Reads objdump text and the ORIGIN<TAB>ERE families in ALLOW; prints SUMMARY, one OK line per origin that holds
# dispatched code and one BAD line per offending function (all of them, grouped by the caller).
# BOLT and the compiler split or clone functions into name.cold.N, name.part.N, name.constprop.N: they belong to name.
# BOLT keeps the pre-rewrite text as the section .bolt.org.text: every rewritten function starts with a jmp to its new
# place and the rest of the old bytes is dead (it disassembles as garbage under the function that precedes it), and a
# function BOLT leaves alone keeps its bytes. Those sections are skipped: the binary BOLT reads is checked with this
# script before BOLT runs (scripts/build-release-binaries.sh), and the live rewritten code is checked in .text.
# shellcheck disable=SC2016
awk_program='
function isa(m) {
  if (m ~ /^v/) return m !~ /^(verr|verw|vm(call|clear|func|launch|load|mcall|ptrld|ptrst|read|resume|run|save|write|xo(ff|n)))$/
  return m ~ /^(k(mov|and|andn|or|xor|xnor|not|add|shiftl|shiftr|test|ortest)[bwdq]|kunpck[bwd][wdq]|andn|bextr|blsi|blsmsk|blsr|bzhi|mulx|pdep|pext|rorx|sarx|shlx|shrx|lzcnt|movbe|adcx|adox)$/
}
function origin_of(sym,   i) {
  for (i = 1; i <= nfam; i++) if (sym ~ fam_re[i]) return fam_origin[i]
  return ""
}
BEGIN {
  nfam = split(ENVIRON["ALLOW"], lines, "\n")
  for (i = 1; i <= nfam; i++) { split(lines[i], kv, "\t"); fam_origin[i] = kv[1]; fam_re[i] = kv[2] }
  name = "[no section]"
}
/^Disassembly of section/ { name = "[section " $4; sub(/:$/, "", name); skip = name ~ /^\[section \.bolt\.org\./; name = name "]"; flagged = 0; next }
/^[0-9a-f]+ <.*>:$/ {
  name = $0; sub(/^[0-9a-f]+ </, "", name); sub(/>:$/, "", name)
  gsub(/\.(cold|part|constprop|isra|lto_priv)(\.[0-9]+)*/, "", name)
  flagged = 0; next
}
/^ +[0-9a-f]+:\t/ {
  split($0, f, "\t"); k = split(f[2], w, / +/); i = 1
  while (i < k && w[i] ~ /^(lock|rep|repz|repnz|repe|repne|bnd|notrack|data16|addr32|cs|ds|es|fs|gs|ss|xacquire|xrelease|rex[.A-Za-z]*)$/) i++
  if (skip || !isa(w[i])) next
  if (!flagged) {
    flagged = 1; org = origin_of(name); leak = org == ""
    if (leak) { bad[++nbad] = name; first[nbad] = w[i]; at[nbad] = f[1]; gsub(/[ :]/, "", at[nbad]) } else { dispatched++; ok[org]++ }
  }
  if (leak) count[nbad]++
}
END {
  printf "SUMMARY %d %d\n", dispatched, nbad
  for (o in ok) printf "OK %s %d\n", o, ok[o]
  for (i = 1; i <= nbad; i++) printf "BAD %s\t%s: %s x%d at 0x%s\n", (bad[i] ~ /::/ ? "Rust code" : "C or assembly symbols"), bad[i], first[i], count[i], at[i]
}'


# AArch64: the same report as above for a function of an extension that a generic ARMv8-A CPU lacks. The instruction
# regexes are passed with -v; a mnemonic is the first word after "ADDRESS:" (GNU objdump and llvm-objdump agree).
# shellcheck disable=SC2016
aarch64_awk_program='
function extension(m, ops) {
  if (m ~ lse) return "LSE atomic"
  if (m ~ later) return "ARMv8.1+ extension"
  if (m ~ pac) return "non-hint pointer authentication"
  if (m ~ mte) return "MTE"
  if (m ~ crypto || (m ~ /^pmull2?$/ && ops ~ /\.1[qd]/)) return "crypto extension"
  if (m ~ sve || ops ~ /(^|[ ,{[])z[0-9]+(\.|[,}\]]|$)/ || ops ~ /(^|[ ,{[])p[0-9]+\/[mz]/) return "SVE or SME"
  if (m ~ /^(f|scvtf|ucvtf)/ && m !~ /^fcvt(l2?|n2?)?$/ && ops ~ /(^|[ ,{[])h[0-9]+|\.[248]h/) return "FP16 arithmetic"
  return ""
}
function origin_of(sym,   i) {
  for (i = 1; i <= nfam; i++) if (sym ~ fam_re[i]) return fam_origin[i]
  return ""
}
BEGIN {
  nfam = split(ENVIRON["ALLOW"], lines, "\n")
  for (i = 1; i <= nfam; i++) { split(lines[i], kv, "\t"); fam_origin[i] = kv[1]; fam_re[i] = kv[2] }
}
/^Disassembly of section/ { flagged = 0; next }
/^[0-9a-f]+ <.*>:$/ {
  name = $0; sub(/^[0-9a-f]+ </, "", name); sub(/>:$/, "", name)
  gsub(/\.(cold|part|constprop|isra|lto_priv)(\.[0-9]+)*/, "", name)
  sub(/::h[0-9a-f]+$/, "", name)
  flagged = 0; next
}
/^ +[0-9a-f]+:[ \t]/ {
  at_text = $0; sub(/^ +/, "", at_text); where = at_text; sub(/:.*/, "", where)
  sub(/^[0-9a-f]+:[ \t]+/, "", at_text); m = at_text; sub(/[ \t].*/, "", m); ops = substr(at_text, length(m) + 1)
  why = extension(m, ops)
  if (why == "") next
  if (!flagged) {
    flagged = 1; org = origin_of(name); leak = org == ""
    if (leak) { bad[++nbad] = name; first[nbad] = m " (" why ")"; at[nbad] = where } else { dispatched++; ok[org]++ }
  }
  if (leak) count[nbad]++
}
END {
  printf "SUMMARY %d %d\n", dispatched, nbad
  for (o in ok) printf "OK %s %d\n", o, ok[o]
  for (i = 1; i <= nbad; i++) printf "BAD %s\t%s: %s x%d at 0x%s\n", (bad[i] ~ /::/ ? "Rust code" : "C or assembly symbols"), bad[i], first[i], count[i], at[i]
}'

re_lse='^((ld|st)(add|clr|eor|set|smax|smin|umax|umin)p?(a|l|al)?[bh]?|swpp?(a|l|al)?[bh]?|casp?(a|l|al)?[bh]?|ldlar[bh]?|stllr[bh]?|sqrdml[as]h)$'
re_later='^(crc32c?[bhwx]|[su]dot|usdot|sudot|[su]smmla|usmmla|ummla|bfdot|bfmmla|bfmlal[bt]|bfcvtn2?|bfcvt|fmlal2?|fmlsl2?|fcadd|fcmla|fjcvtzs|frint(32|64)[xz]|rmif|setf(8|16)|cfinv|axflag|xaflag|sb|ldapr[bh]?|ldapur(s?[bhw])?|stlur[bhw]?)$'
re_pac='^((pac|aut)[id]z?[ab]|pacga|xpac[id]|(ret|bra|brb|blra|blrb|eret|ldr)a[ab]z?)$'
re_mte='^(irg|gmi|addg|subg|ldgm?|stgm|stgp|st2g|stzg|stz2g|stzgm)$'
re_crypto='^(aes[ed]|aes(i)?mc|sha1(c|p|m|h|su0|su1)|sha256(h2?|su0|su1)|sha512(h2?|su0|su1)|eor3|rax1|xar|bcax|sm3.*|sm4.*)$'
re_sve='^(rdvl|addvl|addpl|cnt[bhwd]|ptrue|ptrues|pfalse|whilel[eost]|whilerw|whilewr|rdffr|wrffr|setffr|smstart|smstop|(sq|uq)?(inc|dec)[bhwdp])$'

require_symbols() {
  local disasm=$1
  [[ $(readelf -S -W "${disasm}") == *' .symtab '* ]] ||
    fail 'stripped binary: no function symbols to tell dispatched code from a leak; pass --symbols UNSTRIPPED (same' \
      'build-id) or verify the unstripped binary (--run still checks that it starts)'
  if [[ -n ${symbols} ]]; then
    [[ -n $(build_id "${symbols}") && $(build_id "${symbols}") == "$(build_id "${bin}")" ]] ||
      fail "--symbols ${symbols} does not carry the build-id of the binary"
  fi
}

# report_leaks REPORT WHAT: print the offenders of an awk report and fail, or the per-origin count of dispatched code.
report_leaks() {
  local report=$1 what=$2 dispatched leaks by_origin
  read -r _ dispatched leaks <<<"$(sed -n 1p <<<"${report}")"
  if ((leaks > 0)); then
    printf '%s: %d function(s) use %s outside run-time dispatch (extend\n' "${bin}" "${leaks}" "${what}" >&2
    printf 'DISPATCHED_SYMBOL_PATTERNS in %s only for code that runs behind a CPU check):\n' "$0" >&2
    sed -n 's/^BAD //p' <<<"${report}" | LC_ALL=C sort -s -t$'\t' -k1,1 |
      awk -F'\t' '$1 != kind { kind = $1; printf "%s:\n", kind } { printf "  %s\n", $2 }' >&2
    exit 1
  fi
  by_origin=$(sed -n 's/^OK \(.*\) \(.*\)$/\1 \2/p' <<<"${report}" | LC_ALL=C sort | paste -sd, - | sed 's/,/, /g')
  printf '%s: baseline ISA ok (%d dispatched functions%s)\n' "${bin}" "${dispatched}" "${by_origin:+: ${by_origin}}"
}

allow_list() { printf '%s\n' "${DISPATCHED_SYMBOL_PATTERNS[@]}"; }

x86_64_static_check() {
  local disasm=${symbols:-${bin}}
  [[ $(objdump -f "${disasm}") == *i386:x86-64* ]] || fail 'not an x86-64 ELF binary'
  require_symbols "${disasm}"
  report_leaks "$(objdump -d -C --no-show-raw-insn "${disasm}" | ALLOW=$(allow_list) awk "${awk_program}")" \
    'AVX/BMI/FMA-class instructions'
}

# GNU objdump of an arm64 host decodes AArch64; any other host needs llvm-objdump (or OBJDUMP).
aarch64_disassembler() {
  local candidate
  for candidate in "${OBJDUMP:-}" objdump llvm-objdump; do
    [[ -n ${candidate} ]] && command -v "${candidate}" > /dev/null || continue
    [[ $("${candidate}" -i 2> /dev/null) == *aarch64* || $("${candidate}" --version 2>&1) == *LLVM* ]] && { echo "${candidate}"; return; }
  done
  fail 'no objdump that decodes AArch64 (GNU objdump of an arm64 host, llvm-objdump, or set OBJDUMP)'
}

aarch64_static_check() {
  local disasm=${symbols:-${bin}} objdump_aarch64
  objdump_aarch64=$(aarch64_disassembler)
  require_symbols "${disasm}"
  report_leaks "$("${objdump_aarch64}" -d -C --no-show-raw-insn "${disasm}" | ALLOW=$(allow_list) \
    awk -v lse="${re_lse}" -v later="${re_later}" -v pac="${re_pac}" -v mte="${re_mte}" -v crypto="${re_crypto}" \
      -v sve="${re_sve}" "${aarch64_awk_program}")" 'instructions of an extension above ARMv8-A'
}

# run_check_native: the host runs the architecture of BIN itself; its CPU is not the baseline, so this proves only that
# BIN starts.
run_check_native() {
  "${bin}" version > /dev/null || fail "'version' fails on this host"
  printf '%s: runs natively on this %s host (not the baseline CPU)\n' "${bin}" "$(uname -m)"
}

run_check_emulated() {
  local qemu=$1 cpus cpu err status known
  read -ra cpus <<<"${ISA_QEMU_CPUS:-${default_cpus}}"
  ulimit -c 0 # qemu-user writes a core file into the current directory on SIGILL
  if ! command -v "${qemu}" > /dev/null; then
    ((require == 0)) || fail "runtime check required but ${qemu} is not installed (qemu-user)"
    printf '%s: runtime check SKIPPED: %s is not installed\n' "${bin}" "${qemu}"
    return
  fi
  known=$("${qemu}" -cpu help 2>&1) || true # the CPU list goes to stderr and the exit status is 1
  for cpu in "${cpus[@]}"; do
    grep -Eq "^ +${cpu}( |$)" <<<"${known}" || fail "${qemu} does not list the CPU ${cpu} (ISA_QEMU_CPUS)"
  done
  err=$("${qemu}" -cpu max "${bin}" version 2>&1 > /dev/null) ||
    fail "'version' fails even on the richest qemu CPU (max): a loader, library or sysroot problem (QEMU_LD_PREFIX)," \
      "not the ISA: ${err}"
  for cpu in "${cpus[@]}"; do
    status=0
    err=$("${qemu}" -cpu "${cpu}" "${bin}" version 2>&1 > /dev/null) || status=$?
    ((status == 0)) || fail "'version' fails on qemu CPU ${cpu} (status ${status}, 132 is SIGILL): ${err}"
    printf '%s: runs on qemu CPU %s\n' "${bin}" "${cpu}"
  done
}

aarch64_run_check() {
  if [[ $(uname -m) == aarch64 ]]; then run_check_native; else run_check_emulated "${QEMU_AARCH64:-qemu-aarch64}"; fi
}

# readelf prints nothing for a file that is not an ELF; the x86-64 check then names the problem.
machine=$(readelf -h "${bin}" 2> /dev/null | sed -n 's/^ *Machine: *//p' || true)
if [[ ${machine} == AArch64 ]]; then
  default_cpus=cortex-a53
  aarch64_static_check
  ((run == 0)) || aarch64_run_check
else
  default_cpus='Nehalem Denverton'
  x86_64_static_check
  ((run == 0)) || run_check_emulated "${QEMU_X86_64:-qemu-x86_64}"
fi
