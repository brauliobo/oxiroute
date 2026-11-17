# Releasing

This page records the current release communication path. It does not replace the package recipe or
the test strategy.

## Before A Release

- Run the full Rust and UI gates in [TESTING.md](TESTING.md).
- Run `./scripts/verify/control-plane-contract.sh` to verify the checked OpenAPI artifact is generated
  from the registry and DTO schemas without drift.
- Run `./scripts/verify/lockfiles.sh` so every Cargo and pnpm manifest is checked against its
  committed lockfile.
- Re-check [COMPATIBILITY.md](../COMPATIBILITY.md) against the active runtime, not just crate code.
- Update the relevant user/reference pages and add release notes under `docs/`.
- Render the dashboard GIFs if UI labels or product states changed.
- Inspect generated archives and ensure secrets, `target/`, `node_modules/`, and benchmark artifacts
  are absent. `scripts/verify/release/archive.sh` enforces this list and checks the Git-tracked file
  list when called with `--compare-worktree`.
- Verify the Arch recipe and `.SRCINFO` only after the release asset exists.
- Run `scripts/verify/rtmp/public-api.sh` only when proving the immutable RTMP Phase 0 fixture against
  the matching current 0.4.1 API. Its command semantics are exact equality between the current
  `oxiroute-rtmp` rustdoc graph and `docs/developer/fixtures/rtmp-public-api-phase0.snapshot`. It is
  expected to fail after the authorized 0.5 removals and is not a 0.5 acceptance gate. Do not
  regenerate the Phase 0 fixture. For 0.5 acceptance, require the exact classified generalized RTMP
  delta from `scripts/verify/public/api.sh` and the future pinned `cargo-public-api` semantic
  comparison described below.
- Run `scripts/verify/public/api-baseline-provenance.sh` to regenerate all-features rustdoc JSON from
  exact commit `2d9c5fe66cd096d7a1d8e3bada8d5784b5f97f6c` in an isolated archive under `/tmp/opencode` and
  compare the result byte-for-byte with the five checked generalized baselines. The command pins
  Rust 1.97.1, target `x86_64-unknown-linux-gnu`, canonical schema 4, and cleans the archive on exit.
  It fails rather than installing a missing target. Baseline schema 4 is implemented by the checked
  immutable `docs/developer/fixtures/public-api-canonicalizer-v4.mjs`, whose reviewed SHA-256 is
  `896c406527e412456f4f3a51281ced1363331def95e90b99b086f00726ac39e5`. The executable verifier
  hardcodes that digest, verifies it before execution, copies the verified bytes into the isolated
  directory, and never executes the mutable candidate entrypoint for baseline generation. It also
  pins the candidate entrypoint digest
  `2545bc342e1042f0f4986875fd7f1944f61715f93eecd61cc42e190edd9a08f1`. Run
  `scripts/verify/public/api-baseline-provenance.sh --adversarial-self-test` to prove mutations of
  either source fail digest verification. `--update` is reserved for intentional, reviewed baseline
  regeneration from that same authenticated commit; changing canonicalizer bytes requires separately
  reviewing and updating both hardcoded and documented digests.
- Run `scripts/verify/generation/blueprint/baseline.sh` to regenerate the comprehensive runtime-plan
  behavior fixture from exact commit `2d9c5fe66cd096d7a1d8e3bada8d5784b5f97f6c`. The gate authenticates
  its archived instrumentation and harness with hardcoded SHA-256 digests, compares schema-2
  normalized decisions, acquired service/TLS and pool outputs, exact topology, errors, and acquisition
  trace stop points byte-for-byte against the candidate, and removes its `/tmp/opencode` workspace on
  exit. The reviewed instrumentation digest is
  `ba9a2ad1252ffe96bb4d5a84a40ce226a2a7ae374feeb1241b0549d4feaa11df`; the reviewed harness digest
  is `99b173a270a21f24c5766e1611a854a226892ea794f40c4033406e7f29cc8cbc`, and the generated fixture
  digest is `b83eb1bb086d72b9eae8030aeb9ed1cd799d82fb415fcc2401e0f7bbabccabd3`.
  Run `scripts/verify/generation/blueprint/baseline.sh --adversarial-self-test` to prove mutations of
  either immutable input fail authentication.
- Run `scripts/verify/public/api.sh` for the fast candidate comparison. It uses the identical
  toolchain, target, features, and schema metadata and checks each generated delta against the
  classified `docs/developer/fixtures/*-public-api-0.5.delta` files. The canonicalizer derives
  first-party dependency aliases from each dependency rustdoc root and rejects unresolved private
  definition paths. Baseline and candidate inventories are comparable only when their schema fields
  match. Use `--update` only after every changed item is accounted for in the release notes.
- Before the `0.5` API boundary, also use pinned `cargo-public-api` 0.52.0 to diff the exact `v0.4.1`
  release source against the release candidate as an independent cross-check.

## Version Sources

The workspace version is in the root `Cargo.toml`; the server exposes it through `version`. Release
notes live in `docs/RELEASE_NOTES_<version>.md` (the publish job refuses a release without them and appends the assets section). Notes written ahead of a release are named `docs/RELEASE_NOTES_<version>_DRAFT.md` and renamed when the
release is cut. The Arch recipe pins the release archive and checksum.
Keep all three aligned.

## Package Path

`scripts/create-release-archive.sh` creates a deterministic source archive from committed `HEAD`,
excluding the Arch recipe and benchmark report artifacts. `scripts/verify/release/archive.sh` checks
the root prefix, every committed lock/license input, forbidden artifact and secret-shaped paths,
private-key and high-signal credential content, and optional committed file-list equality.
`packaging/arch/build-local.sh` reuses those checks before invoking `makepkg`; it still checks the
recipe checksum. The package installs the daemon, management client, importer, service metadata,
examples, and documentation but does not enable or start systemd automatically.

Before creating an archive, verify the version metadata and release notes:

```sh
./scripts/verify/release/version.sh 7.0.0
./scripts/create-release-archive.sh /tmp/oxiroute-7.0.0.tar.gz 7.0.0
./scripts/verify/release/archive.sh /tmp/oxiroute-7.0.0.tar.gz 7.0.0 --compare-worktree
```

Read [packaging/arch/README.md](../../packaging/arch/README.md) before changing service permissions,
recording roots, or package files.

## Release Binaries, PGO, BOLT And The Abort Flavour

`.github/workflows/release.yml` builds ONE binary set per architecture (`oxiroute`, the `oxr` alias and
`oxiroute-worker-launcher`) with the reusable workflow `.github/workflows/release-binary.yml`: `binary-x86_64` and
`binary-aarch64` run in parallel on a native runner each, do not wait for the archive job, and every tarball and every
`.deb` and `.rpm` of the architecture is that one binary repackaged. The `x86_64-unknown-linux-gnu` binary is
profile-guided, which cuts user-space instructions per request by 14 to 27%, and laid out by BOLT only on request (BOLT is
opt-in, see below); the `aarch64-unknown-linux-gnu` binary is profile-guided and not laid out by BOLT.
`scripts/package/main.sh binary ARCH VERSION OUT_DIR` builds it inside an `almalinux:9` container, the oldest glibc of every
supported system (2.34: Debian 12 and 13, Ubuntu 22.04 and 24.04, the RHEL family 9 and 10, Amazon Linux 2023), and
`scripts/verify/glibc_floor.sh` fails the build when a binary needs a glibc symbol version above the floor in
`scripts/package/families.sh`. The container runs `scripts/build-release-binaries.sh VERSION OUT_DIR`, which calls
`benchmarks/scripts/pgo.sh` and `benchmarks/scripts/bolt.sh`. The script has two stages: `STAGE=pgo` (steps 1 to 3, the one
that fixes the libc the binary links) and `STAGE=layout` (step 4 and the tarball). With BOLT the container stops after
`STAGE=pgo` and `scripts/package/main.sh layout` runs the second stage on the runner, because the pinned BOLT is an Ubuntu 24.04
package that does not run on el9's libc. BOLT rewrites the code and does not change the glibc a binary needs, and the floor
is checked again on the rewritten binary. Without BOLT the container runs both stages (`STAGE=all`):

1. `pgo.sh train` (only when the profile of the key is not in the cache, see "The profile" below): instrumented build (`-Cprofile-generate`), replay of 24 R2 data-path lanes (the ten core lanes
   with weight 4, fourteen feature lanes with weight 1) through the instrumented proxy with
   `benchmarks/r2-data-path/run.sh --train` (loopback only, no `perf`, so it runs on GitHub runners),
   `llvm-profdata merge` with the weights into one profile.
2. `pgo.sh build`: release build with `-Cprofile-use` and the same cargo command. It parses the compiler's
   `-pgo-warn-missing-function` diagnostics and fails when more than 5% of the profile's functions were not matched,
   so a profile the compiler ignored cannot be published as a PGO build.
   Every build of the script (instrumented, profile-use, plain) appends the same `--remap-path-prefix` flags for the
   checkout, the cargo home and `PGO_WORK`, so the binary embeds no runner or home-directory path and the profile still
   matches. `scripts/build-release-binaries.sh` then runs `scripts/verify/binary-paths.sh` and fails when a binary
   contains `/home/`, `/runner/work`, the checkout or the cargo home. The vendored OpenSSL records its own build
   directory, which no rustc flag can remap, so `PGO_WORK` must lie outside `/home` (the workflow uses `/tmp`).
3. `pgo.sh smoke`: `oxiroute config check` of a generated configuration and two lanes of proxied requests through the
   binary; any load error fails the job.
4. `bolt.sh all` (opt-in: skipped with `BOLT=0`, which is what a tag does unless the repository variable `RELEASE_BOLT` is `1` or a
   manual run sets `bolt`; it measured 0.2% fewer instructions per request, below the 2% that justifies the minutes): the PGO binary is linked with `-Wl,-q` (relocations kept; code generation is
   unchanged, so the profile still matches), `llvm-bolt -instrument` adds counters (no `perf`, no LBR: it works in the
   virtual machines of hosted runners), ten lanes replay through the counted binary, and `llvm-bolt` rewrites the PGO
   binary (block order, function order, hot/cold splitting). The stage fails when fewer than 1000 functions got a
   profile, when the output grew by more than 50%, or when it is no longer PIE, lost `GNU_RELRO` or `BIND_NOW`, or does
   not start. Then `smoke` runs again on the rewritten binary. The job `binary-tests` runs
   `scripts/test/release/binary.sh` on the tarball's binary (the BOLT layout and the abort flavour): the whole
   workspace test suite with that binary as `CARGO_BIN_EXE_oxiroute`, so every process, supervised, CLI and
   panic-containment test drives the shipped bytes, in parallel with the packages. `publish` waits for it. Any failure fails
   the release; nothing falls back to the PGO binary.

BOLT comes from `scripts/install-bolt.sh DIR`: the Ubuntu 24.04 `bolt-18` and `libbolt-18-dev` 18.1.3-1 packages are
downloaded and used only when their SHA-256 matches the values pinned in the script (a mismatch or an unreachable
mirror fails the step). The workflow runs it before the layout stage and exports `BOLT_BIN_DIR`. To update BOLT, take the new
package hashes from the `Packages` index of the Ubuntu archive and change the pins in the script. BOLT is a pipeline
tool only, it is not linked into the product. BOLT bugs are binary-level and invisible to a normal `cargo test`: that
is why the suite runs against the final artefact.

The tarball `oxiroute-VERSION-TARGET.tar.gz` (layout of the Arch package: `bin/oxiroute`, `bin/oxr`,
`lib/oxiroute/oxiroute-worker-launcher`, `LICENSE`, `BUILD-INFO.json`), its `.sha256` and `.build.json` (version,
commit, `rustc --version`, `pgo`, `bolt`, `panic`, SHA-256 of the profile) are uploaded as workflow artifacts and
attested; the publish job attaches them to the tag's release and appends a line stating how the binaries were built to
the release notes. A training, profile, BOLT or test failure fails the job: there is no silent plain build. A manual run
can start with the `pgo` input set to `false` (a plain build, said so in the notes line, and refused by the publish
job) or `bolt` set to `true` (BOLT is off by default).

### aarch64

The `aarch64-unknown-linux-gnu` tarball is built on the hosted `ubuntu-24.04-arm` runner, in an arm64 `almalinux:9`
container, by the same script (`PGO_TARGET=aarch64-unknown-linux-gnu`): the instrumented build, the 24 training lanes and the
profile-use build run on the native architecture, with the same profile-match gate and the same glibc 2.34 floor. The
workflow also runs the shipped binary natively with `scripts/verify/baseline-isa.sh --run --require`; the runner CPU is not
the ARMv8-A baseline, so the instruction check of the disassembly is the gate, as for the other flags below.
**BOLT is off on aarch64** (the decision for the first release with packages): `scripts/install-bolt.sh` pins the
amd64 Ubuntu packages, and no BOLT replay, rewrite or test run has been verified on aarch64 hardware, so the script
refuses `BOLT=1` for that target (and `FLAVOUR=v3`) instead of building something unverified. The abort and x86-64-v3
flavours stay x86_64-only tarballs. The release gates hold for aarch64 as well, adapted to the architecture rather than
skipped: `reject_isa_flags aarch64-unknown-linux-gnu` refuses `-march=` other than `armv8-a`, `-mcpu=` other than
`generic` and any `-Ctarget-cpu` or `-Ctarget-feature` (so `-mcpu=native` fails the build), `scripts/verify/binary-paths.sh`
is architecture independent, and `scripts/verify/baseline-isa.sh` disassembles an AArch64 binary and fails on LSE, CRC32,
crypto, dot-product, FP16, SVE and the other instructions above ARMv8-A outside the run-time dispatched code (libgcc
and compiler-builtins outline atomics, OpenSSL, aws-lc, ring, zlib-ng, zstd, jemalloc, run-time detected Rust crates), and
`--run` executes `version` on the native host or under `qemu-aarch64 -cpu cortex-a53`.
`scripts/test/verify/baseline-isa.sh` tests them with assembled fixtures (aarch64 fixtures need an arm64 host or clang,
lld and llvm-objdump). The dispatch allow-list was written from the instruction classes and the symbol families of the
x86-64 list and has not yet met a real aarch64 release binary: if the first aarch64 build lists a function that sits
behind a hardware check, extend `DISPATCHED_SYMBOL_PATTERNS` with the family and its evidence; a failure is the guard
working, not a reason to skip it.

### Abort flavour

The job `binary-extras` builds `FLAVOUR=abort` next to the default binary, in parallel with it: cargo profile `release-abort` (`inherits = "release"`, `panic = "abort"`),
its own profile data (unwinding changes the control-flow hashes, so the default profile matches only 78% of it), the
same BOLT stage, and the artefact `oxiroute-VERSION-TARGET-abort.tar.gz` with its own checksum, build record and notes
line. A tag always publishes it; a manual run can skip it with `abort_flavour: false`. The default artefact, the Arch
recipe and the source archive keep `panic = "unwind"`.

The trade-off is the reason it is a separate artefact. With unwinding, tokio and Pingora contain a panic to one
connection, the listener service wrapper (`catch_unwind` in `listener_services.rs`) turns a panic into a failed
generation that the supervisor rolls back, and the RTMP recorder reports `worker_panicked`. With `panic = "abort"` the
first panic anywhere ends the worker process: every connection of that worker closes, the supervisor sees a worker exit
and treats the generation as failed (the same path as a killed worker), and recorder panics are no longer reported
because the process is gone. It is meant for operators who prefer fail-fast with supervisor restarts and accept that one
bad request can take down every connection of the worker; it measures 3 to 9% fewer instructions per request.

### C flags and the x86-64-v3 flavour

Every flavour compiles the C dependencies (the vendored OpenSSL above all, also aws-lc, Lua, jemalloc) with `-O3`
(`CFLAGS_x86_64_unknown_linux_gnu` exported by `scripts/build-release-binaries.sh`; the flags are recorded as `c_flags`
in `BUILD-INFO.json`): 7% to 10% fewer instructions per TLS handshake.
`FLAVOUR=v3` adds `-march=x86-64-v3` and publishes `oxiroute-VERSION-TARGET-x86-64-v3.tar.gz`: another 2.5% to 4% on
handshakes, and the binary does not start on a CPU without AVX2 and BMI2. It is therefore never part of a tag run and
never the default artefact; start the release workflow manually with `v3_flavour: true` and `publish: true` to attach it. Source builds
(`cargo build`, the Arch recipe) do not set the flags.

Requirements: the toolchain that trains must be the toolchain that builds. The profile is keyed by symbol names, and
rustc hashes its own version and the unit configuration into every symbol, so the jobs install the pinned Rust 1.97.1
with the `llvm-tools` component (its `llvm-profdata` reads that compiler's raw format) and nothing else compiles
between training and the final build. The profile is a cache entry, never committed: the source archive, its checksum and
`scripts/create-release-archive.sh` are unaffected, and the Arch recipe keeps building from source without a profile.

### The profile: reuse, cache keys, refresh and retrain triggers

Training is not repeated per release. The profile of an architecture and flavour (`unwind` and `abort` have their own,
unwinding changes the control-flow hashes; `aarch64` has its own) is a pure function of its inputs, so
`release-binary.yml` asks `release-profile.yml` for it and only the missing ones are trained:

1. `scripts/package/main.sh profile key ARCH FLAVOUR` prints the key, `pgo-ARCH-FLAVOUR-vVERSION-HASH`. HASH is the first 16
   hex digits of a SHA-256 over the pinned toolchain (`rust_toolchain` of `scripts/package/families.sh`), the target, the cargo
   profile (so the flavour), the lane mix and scale, and the content of `Cargo.toml` (build settings), `Cargo.lock`, `crates/`,
   `vendor/`, `benchmarks/scripts/pgo.sh` and `benchmarks/r2-data-path/`, leaving out `tests/`, `benches/` and `*.md` files and
   blanking the workspace version in the manifests and the lock file. UI, documentation, tests and a version-only bump do not
   change HASH (`benchmarks/scripts/test-pgo.sh` proves it, as it proves that a source, dependency or build-setting change does).
   `profile prefix` prints `pgo-ARCH-FLAVOUR-vVERSION-`.
2. The version is the first component, not part of HASH, because it decides whether a profile fits at all: rustc hashes the
   package version into the symbol of every workspace function (measured: the same crate built at 6.5.2 and 6.5.3 has 35 of 35
   exported symbols renamed), and a profile is matched by symbol name, so a profile trained at another version matches no
   workspace function and the gate rejects it. A version bump is therefore always a new profile; the refresh below makes it
   before the tag.
3. The `lookup` job restores `profile/oxiroute.profdata` from `actions/cache`. A release build asks for the exact key and,
   when that is missing, for the newest key under the prefix of its version (`restore-keys`): normally the last refresh of
   `main`, whose sources can differ a little from the tag. A hit of either kind stops there: no instrumented build, no lanes.
   Only when nothing of that version exists the `train` job runs `scripts/package/main.sh profile train` (the instrumented
   build, the 24 lanes and the merge in the glibc-floor container, `STAGE=train`) and saves the exact key. The refresh asks for
   the exact key only, so it trains whenever HASH moved.
4. The binary job restores the key that the lookup settled on with `fail-on-cache-miss: true`, mounts the file into the build
   container (`PGO_PROFILE_HOST`, `PGO_PROFILE_GIVEN=1`) and goes straight to the profile-use build. The existing gate
   (`PGO_MAX_MISSING_PCT`, 5%) decides whether the profile fits the code that is built: a mismatch exits with status 3 and the
   message "train it again". It never produces a plain build; the job fails and the profile must be trained again (below).

`.github/workflows/profile-refresh.yml` keeps the profiles of `main` warm for the three profiles of a tag (x86_64 unwind and
abort, aarch64 unwind). It runs automatically on every push to `main` that touches `Cargo.toml` or `Cargo.lock` (where a
version bump, a toolchain or a dependency change lands), every night (03:17 UTC, which also renews the entries that GitHub evicts
after seven days without access) and on demand. A tag run restores the entries that `main` saved; the entries a tag run saves
are visible to that tag only.

Retrain triggers: a change to HASH (any crate or vendored source other than tests, benches and documents, a dependency
version in `Cargo.lock`, a build setting in `Cargo.toml`, the toolchain pin, `benchmarks/scripts/pgo.sh`, the lane list or the
scale, anything in `benchmarks/r2-data-path/`, the target, the flavour) trains on the next refresh; a new version trains too.
A tag whose HASH moved since the last refresh still uses the newest profile of its version if the gate accepts it (at most 5%
of the profile's functions unmatched), which is how a hot-path commit after a night's run does not force a half-hour
training. The one manual fallback: if the tag run fails the gate (status 3) or you tag within minutes of the version bump, start
`Profile refresh` on `main`, wait for the three legs, and re-run the tag workflow; a tag run that finds nothing of its version
trains inside its own run instead (the instrumented build, about half an hour on a hosted runner, is the larger half of a cold leg).
To force a retrain, delete the entry (`gh cache delete KEY`, the key is in the job summary of the `lookup` job) and run `Profile
refresh`. A gate failure with an unchanged key means an input is missing from the key: fix `key_inputs` in
`benchmarks/scripts/pgo.sh`, do not raise `PGO_MAX_MISSING_PCT`.

Local: `PGO_PROFILE_GIVEN=1 PGO_PROFILE=/path/to/oxiroute.profdata scripts/build-release-binaries.sh VERSION OUT` builds with a
profile you trained earlier (`STAGE=train` leaves `OUT/oxiroute.profdata`); `benchmarks/scripts/test-pgo.sh` tests the key, the
gate and the skip. Every stage of the scripts prints `stage-time NAME SECONDS` (`STAGE_TIMES=FILE` appends them, the jobs
publish them as a table in the step summary).

To regenerate locally with the same compiler:

```sh
export PATH=$HOME/.rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu/bin:$PATH   # rustup component add llvm-tools
BOLT_BIN_DIR=$(scripts/install-bolt.sh /var/tmp/oxiroute-bolt-tools) \
  ./scripts/build-release-binaries.sh 7.0.0 /path/to/oxiroute-release
BOLT=0 ./scripts/build-release-binaries.sh 7.0.0 /path/to/oxiroute-pgo          # PGO only
PGO=0 BOLT=0 ./scripts/build-release-binaries.sh 7.0.0 /path/to/oxiroute-plain  # plain build of the same command
FLAVOUR=abort BOLT=0 ./scripts/build-release-binaries.sh 7.0.0 /path/to/oxiroute-abort
```

`PGO_WORK` (default `/var/tmp/oxiroute-pgo`: a disk directory outside `/home`, because the vendored OpenSSL records its build directory; the build directories are several GB) moves the
scratch data, `PGO_LANES` and `PGO_SCALE` shorten the training for a local check of the flow; a profile from a shortened
or different workload still builds and passes the gate but gives less than the 14 to 27%. A build on your own machine
links that machine's glibc; the release binary is built by `scripts/package/main.sh binary` in an `almalinux:9` container
(glibc 2.34) so that the tarballs and the packages run on every supported distribution.

## Runtime CPU dispatch and the ISA guard

Every artefact but `x86-64-v3` is a generic x86-64 binary: SSE2, SSSE3, AVX2 and AVX-512 code paths are compiled as
`#[target_feature]` variants and chosen at run time by the `oxiroute-cpu` crate, on by default. The original failure was
a `SIGILL` on an Intel Celeron J4125 (SSE4.2 and POPCNT, no AVX) from a build that had baked AVX in. For triage and
tests, `OXIROUTE_CPU_LEVEL=scalar|sse2|sse42|avx2|avx512` forces a level; it can only lower the level and never exceeds what
the CPU supports.

Three gates keep a CPU target out of the shipped bytes (the aarch64 binary has the same gates against ARMv8-A, see
[aarch64](#aarch64); the text below describes x86-64):

- `scripts/lib/reject-isa-flags.sh` (`reject_isa_flags`, called by `scripts/build-release-binaries.sh`,
  `benchmarks/scripts/pgo.sh` and, as an inline copy for x86-64, `packaging/arch/PKGBUILD`) fails the build with a one-line message
  when `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`, `CARGO_BUILD_RUSTFLAGS` or `CARGO_TARGET_*_RUSTFLAGS` contain any
  `-Ctarget-cpu` or `-Ctarget-feature`, or when `CC`, `CXX`, `CFLAGS`, `CXXFLAGS`, `CPPFLAGS`, `CFLAGS_<target>` and
  friends (the cc crate inherits them for OpenSSL, aws-lc and Lua) carry `-march=`, `-mavx*`, `-msse4*`, `-mbmi*`, `-mfma`
  and the like. Only `-march=x86-64`, `-march=x86-64-v1` and `-mtune=generic` (the makepkg defaults) pass; nothing is
  stripped silently. Arch users whose `makepkg.conf` sets `-march=native` set generic flags in `~/.makepkg.conf`.
- `scripts/verify/baseline-isa.sh BIN` disassembles the binary and fails on VEX/EVEX, BMI, FMA, LZCNT and MOVBE
  instructions in any function that is not runtime-dispatched (`oxiroute_cpu::`, memchr, aho-corasick, aws-lc and ring
  assembly, zlib-ng, zstd, glibc, ... or a symbol that names its ISA); the allow-list is `DISPATCHED_SYMBOL_PATTERNS` in the
  script, one `family ORIGIN REGEX` entry per body of third-party code with the CPU check that gates it (for aws-lc: the
  `L<routine>_<label>` symbols of its s2n-bignum and ML-KEM assembly, and AES-GCM-SIV). It lists every offender, grouped
  into Rust code and C or assembly symbols, and counts the accepted functions per origin. It skips the dead pre-rewrite section `.bolt.org.text` of a BOLT output (the
  build checks the binary BOLT reads before BOLT runs). A stripped binary fails closed: pass `--symbols UNSTRIPPED` (same build-id). It runs on the release binaries
  in `build-release-binaries.sh`, not for the `v3` flavour.
- `scripts/verify/baseline-isa.sh --run BIN` also runs `BIN version` under `qemu-x86_64 -cpu Nehalem` and
  `-cpu Denverton` (a Goldmont-class Atom: SSE4.2, no AVX; qemu has no Goldmont Plus, the J4125 core) and fails on a
  non-zero exit or `SIGILL`. Without `qemu-x86_64` (package `qemu-user`) it prints `SKIPPED` and exits 0; `--require`
  makes that exit 1. A dynamic binary needs the host loader or `QEMU_LD_PREFIX=<baseline sysroot>`; a host glibc built
  for x86-64-v3 raises `SIGILL` before the binary runs.

`scripts/test/cpu-targets.sh` builds and executes the CPU and HTTP/1 scanner tests for generic x86-64 and the
native build host. The generic test executables also run under QEMU with baseline, SSSE3 and AVX2 CPUs, including an
AVX2 CPU with unrelated POPCNT and BMI features masked. Separate masks of SSE3, SSSE3 and SSE4 verify that
selection respects Rust’s target-feature dependencies and lowers the backend when needed. Each emulated run asserts
both the detected and selected backend; setting a requested level above the emulated hardware must not execute
unsupported instructions.
`CPU_TEST_CPUS` overrides the realistic matrix as space-separated `CPU=LEVEL` pairs; `CPU_TEST_MASKED_CPUS` overrides
the additional feature masks, which apply only to CPU primitives. HTTP/1 differential tests use the real CPU profiles:
the external `httparse` oracle assumes AVX2 implies its compiler-required SSE features and cannot run on an inconsistent
CPUID combination. These masks establish primitive dispatch coverage, not full application coverage.
AVX-512 requires a capable native host or an emulator that implements it; requesting it on an AVX2-only emulator
does not establish AVX-512 coverage.
For a host whose libraries require x86-64-v3, set `QEMU_LD_PREFIX=/path/to/baseline-sysroot` to compatible generic
loader, glibc and compiler runtime libraries. This changes the emulated loader; it does not alter the host libraries.
`cpu-targets.sh` fails with that instruction when the host loader raises `SIGILL` on a baseline CPU model before any
test runs; a sysroot without its own `/etc/ld.so.cache` also needs
`QEMU_SET_ENV=LD_LIBRARY_PATH=/lib/x86_64-linux-gnu` so the host cache does not resolve the host libraries.

`scripts/test/verify/baseline-isa.sh` tests the guard and the verifier offline with assembled fixtures (CI job
"Release archive policy").

## Release Assets And Packages

`scripts/package/` owns the asset set; the workflows `release.yml`, `release-binary.yml` and `release-packages.yml` only call
it. A release carries, for the version `V` (no leading `v`):

| Asset | Built by | Notes |
| --- | --- | --- |
| `oxiroute-V.tar.gz`, `SOURCE-PROVENANCE.json` | `archive` | Deterministic source archive and provenance |
| `oxiroute-V-x86_64-unknown-linux-gnu.tar.gz` | `binary-x86_64` | PGO and BOLT, glibc 2.34; also `-abort` (`binary-extras`, a tag always) and `-x86-64-v3` (dispatch only) |
| `oxiroute-V-aarch64-unknown-linux-gnu.tar.gz` | `binary-aarch64` | PGO, no BOLT, glibc 2.34 |
| each tarball's `.sha256` and `.build.json` | `binary-*` | `pin-bin-release.sh` reads the `.sha256` |
| `oxiroute_V_amd64.deb`, `oxiroute_V_arm64.deb` | `packages-*` | Repackages the tarball binary with dpkg-buildpackage on Debian 12 |
| `oxiroute-V-1.el9.x86_64.rpm`, `oxiroute-V-1.el9.aarch64.rpm` | `packages-*` | Repackages it with rpmbuild on AlmaLinux 9 |
| `oxiroute-V-1.amzn2023.x86_64.rpm`, `oxiroute-V-1.amzn2023.aarch64.rpm` | `packages-*` | Repackages it with rpmbuild on Amazon Linux 2023 |
| `<package>.build.json` | `packages-*` | The build record of the binary inside the package: identical to the record of the tarball of its architecture, which `publish` checks |
| `SHA256SUMS` | `publish` | One file for every asset above |

The `.deb` installs on Debian 12 and 13 and Ubuntu 22.04 and 24.04; the `el9` `.rpm` on AlmaLinux 9 and Rocky Linux 9, and
the test matrix also installs it on AlmaLinux 10 (a reported result, not a promise of the recipe); the `amzn2023` `.rpm` on
Amazon Linux 2023. They carry the same binary, so the families differ only in packaging metadata.

A pre-release version `7.0.0-rc.1` is `7.0.0~rc.1` in the package versions, so it sorts below `7.0.0`.

### Cut a release

1. Set the workspace version, write `docs/RELEASE_NOTES_V.md` (the release text; the convention and the check of
   `scripts/verify/release/version.sh` are unchanged), pin `packaging/arch/PKGBUILD` and `.SRCINFO` to the new source
   archive (`packaging/arch/build-local.sh`), and merge. The version lives in `Cargo.toml`,
   `crates/oxiroute-forward-proxy/Cargo.toml`, `Cargo.lock` and `fuzz/Cargo.lock` (`cargo update -w --offline`, plus
   `--manifest-path fuzz/Cargo.toml`; both only change workspace versions), the `info.version` of
   `contracts/*.openapi.json` and of `crates/oxiroute-server/src/registry/contract.rs` and
   `src/rtmp/api/contract.rs`, `ui/package.json`, `remotion/package.json`, `website/index.html`, the Arch recipes and
   their READMEs under `packaging/arch/` (the local README copy is checksummed in both recipes), the version lines of
   `README.md`, `docs/COMPATIBILITY.md` and `docs/PRODUCT_SPEC.md`, and the notes link of `docs/README.md`. The
   checksum of the Arch binary tarball is the one thing the release flow fills afterwards (step 4).
2. A version bump on `main` changes `Cargo.toml` and `Cargo.lock`, which starts `Profile refresh`
   ([the profile](#the-profile-reuse-cache-keys-refresh-and-retrain-triggers)): rustc hashes the version into every workspace
   symbol, so the profile of the previous version matches nothing and the three profiles of the new version (x86_64
   unwind and abort, aarch64 unwind) must be trained before the tag. Wait for the three legs of that run to finish, then
   create the tag; a tag created within minutes of the bump trains inside its own run instead, about half an hour longer.
   Tag `vV` and push the tag. The `changes` job verifies the version metadata and the notes first, so a wrong tag fails in
   a minute, not after the builds. The source archive excludes `packaging/arch`, so its checksum in the PKGBUILD stays valid
   only while nothing else changes between the pin and the tag; the `archive` job fails on a mismatch.
3. `archive`, `binary-x86_64`, `binary-aarch64` and `binary-extras` start together; `packages-x86_64` starts the moment
   `binary-x86_64` is done and `packages-aarch64` the moment `binary-aarch64` is done (no barrier between the
   architectures, and none on the archive job); `binary-tests` starts when the x86_64 binaries are done. Each build job
   attests its files. `publish` runs only when all of them succeeded (`binary-extras` and `binary-tests` may be skipped only
   when the run has nothing for them): `scripts/package/main.sh assets sums dist V` refuses a missing or empty asset, a
   build record that does not say `"pgo": true`, a package that is not the binary of its architecture's tarball, a tarball
   that does not match its `.sha256`, writes the one `SHA256SUMS` and checks it. A new release is created as a
   draft, receives every asset and is published last, so no visitor sees a partial release. The release text is the notes
   file plus the generated "Assets" section.
4. After the assets are public, run `packaging/arch/pin-bin-release.sh V` and commit it.

Re-publishing an existing tag: run the workflow manually with the tag and `publish: true`. The source archive and its
provenance are verified byte for byte and not uploaded again; every other asset and `SHA256SUMS` are replaced.
Without `publish` a manual run builds and tests everything and publishes nothing.

A pull request, or a push to `main`, that changes a file the packages depend on (`recipe_paths_re` in
`scripts/package/families.sh`) runs the same jobs as a dry run: `PGO=0` and `BOLT=0`, only the default flavour, no
`binary-tests`, nothing attested or published, and the publish job is skipped. The plain build keeps its dependency objects
in the workflow cache, so the next dry run compiles only the workspace crates. A newer push cancels the dry run it
replaces; a tag or a manual run is never cancelled. Any other change ends after the first job.

### What a package installs

The installed layout is `packaging/arch/oxiroute.{service,sysusers,tmpfiles,modules,env,kdl,lua}`, copied unchanged
into the Debian recipe (`packaging/deb/debian`, debhelper 13) and the RPM spec (`packaging/rpm/oxiroute.spec`): the
test compares every installed file with the file of `packaging/arch`. Also installed: `/usr/bin/oxiroute`, the `oxr`
symlink, `/usr/lib/oxiroute/oxiroute-worker-launcher`, and `/usr/share/doc/oxiroute/` (install guide, management CLI guide,
`BUILD-INFO.json`, license). The three files in `/etc/oxiroute` are conffiles (deb) and `%config(noreplace)` (rpm). The
packages enable and start nothing (`dh_installsystemd --no-enable --no-start`, `%systemd_post` with the distribution
preset, which enables nothing); a running service is restarted on upgrade as distribution policy says. The `oxiroute` user comes from
sysusers (the shell field is `-`, the platform's nologin, not a path that exists on one distribution only), the state
directory from tmpfiles. `remove` keeps configuration and state; `apt purge` also deletes `/etc/oxiroute`,
`/var/lib/oxiroute` and `/run/oxiroute` and keeps the system user. RPM has no purge: a modified configuration file stays as
`.rpmsave` and `/var/lib/oxiroute` stays. Dependencies: `ca-certificates`, `systemd`, and what the dependency
tool computes from the binary (glibc, libgcc); OpenSSL is linked statically, kTLS and io_uring need kernel features.
`certbot` is suggested. No `Provides` or `Conflicts`: the Arch `oxiroute-bin` conflicts with the Arch `oxiroute`
because both own the same Arch package name, and nothing else ships the `oxiroute` name on Debian or RPM systems.

### Local commands

Needs docker or podman (`PACKAGE_RUNTIME` selects). Resource controls: `PACKAGE_CPUSET=0-7` (container CPUs),
`PACKAGE_NICE=10`, `PACKAGE_JOBS=3` (cargo jobs), `PACKAGE_CACHE=DIR` (toolchain and cargo registry between runs).
The workflow is these commands in this order; a package needs the tarball of its architecture in `OUT_DIR` and takes
seconds to assemble, the binary build is the one slow step:

```sh
PGO=0 scripts/package/main.sh binary x86_64 7.0.0 dist       # the one binary, on the el9 glibc; plain: ONLY for local smoke tests
scripts/package/main.sh binary x86_64 7.0.0 dist             # profile-guided (the release build; STAGE=pgo leaves dist/stage for BOLT)
scripts/package/main.sh build el9 x86_64 7.0.0 dist          # assembles the package from dist/oxiroute-7.0.0-x86_64-unknown-linux-gnu.tar.gz
PGO=0 scripts/package/main.sh test el9 x86_64 dist/oxiroute-7.0.0-1.el9.x86_64.rpm
PGO=0 scripts/package/main.sh all amzn2023 x86_64 7.0.0 dist # assemble, then test
PACKAGE_IMAGES=ubuntu:22.04 PGO=0 scripts/package/main.sh test deb x86_64 dist/oxiroute_7.0.0_amd64.deb   # one image only
scripts/package/main.sh test deb x86_64 dist/oxiroute_7.0.0_amd64.deb old/oxiroute_6.9.0_amd64.deb   # also upgrade
scripts/package/main.sh selftest                             # offline: asset set, refusals, matrices, wiring, pins
ALLOW_PLAIN=1 scripts/package/main.sh assets verify dist 7.0.0   # a local dry run; the workflow never sets ALLOW_PLAIN
```

`PGO=0` writes `"pgo": false` into the build record, which `assets` and the publish job refuse. `binary` never applies BOLT
itself (see above; `main.sh layout` does, on a host that has it). The containers run without the default seccomp profile
because it blocks io_uring, the default data plane, and a profile trained on the epoll plane would leave the code of every
listener cold. An aarch64 binary and package are built natively (the scripts refuse otherwise); on an x86_64 host `test` of
an aarch64 package runs under qemu-user binfmt emulation if the runtime has it registered, which proves the layout and the
installation, not native code.

### Verify a download

```sh
sha256sum --check --ignore-missing SHA256SUMS     # in the directory with the downloaded asset
gh attestation verify oxiroute_V_amd64.deb --repo brauliobo/oxiroute
```

### Build time, budget and caching

The release is one compile per architecture, not one per package: a build of every family on both architectures would
compile the same code six times, and a package is the binary plus metadata, so the six packages and the tarball of an
architecture share one binary. Every stage of the build prints `stage-time NAME SECONDS` and the jobs publish them in the
step summary. Measured on this repository with 8 CPUs of a workstation (`taskset`, `-j3`, cold target directories, Rust 1.100,
the harness on other CPUs); a hosted 4-vCPU runner is taken as 2 times slower, which is an extrapolation, not a measurement:

| Stage | Before, local s | Before, hosted est. | After |
| --- | ---: | ---: | --- |
| Instrumented build (dependencies and fat-LTO link) | 792 | 26 min | 0 on a profile hit (a miss trains once, nightly) |
| Load tool build | 70 | 2.3 min | beside the profile-use build, off the path |
| 24 training lanes + merge | 16 | 0.5 min | 0 on a hit |
| Profile-use build (fat LTO, `codegen-units=1`) | 548 | 18 min | unchanged, now the longest stage |
| of which dependencies, up to the first workspace crate | 196 | 6.5 min | unchanged (see below) |
| of which final `oxiroute` fat-LTO codegen, one thread | 237 | 8 min | unchanged |
| BOLT: instrument 37, replay 10 lanes 8, rewrite 8, guards 0 | 54 | 2 min (+ Rust, pnpm, rust-cache, tool build on the host: 3 to 4 min) | opt-in; host setup reduced to Rust, the tool travels in `stage/` |
| Workspace tests against the binary: test build, cold (shared with a build) | 1054 | 35 min, cached dependencies cut it | unchanged, in parallel with the packages, required by `publish` |
| Workspace tests: run | 471 | 16 min | unchanged, same job |
| Dry run build (`PGO=0`): fat LTO is unmeasured, no LTO with 16 codegen units | 317 | 10.5 min cold | dependencies cached, no LTO |
| `selftest` of the package scripts | not measured | about 2 min | its own job, off the path |

Quality, instructions per request (the harness, `h1_b1k`, `tls_h1_keepalive`, `h2_b1k_c4s16`, `f_fixed`, 3 repetitions):

| Variant | Build | h1_b1k | tls_h1_keepalive | h2_b1k_c4s16 | f_fixed |
| --- | ---: | ---: | ---: | ---: | ---: |
| PGO, fat LTO, 1 codegen unit (shipped) | 548 s | 11498 | 26079 | 28667 | 6507 |
| the same with `-Wl,-q` (the BOLT build) | 598 s | 11540 | 26104 | 28759 | 6512 |
| BOLT-rewritten | + 54 s | 11512 | 26063 | 28748 | 6476 |
| PGO, thin LTO, 16 codegen units, trained with the same configuration | 363 s (instrumented 403 s) | 11929 (+3.7%) | 26615 (+2.1%) | 29113 (+1.6%) | 6517 (+0.2%) |

Decisions from these numbers: the reusable profile removes the instrumented build, the lanes and the tool build
(about 29 minutes on a hosted runner, the larger half of a leg). Thin LTO saves 235 s but costs up to 3.7% instructions per
request on the data path, more than the 1% allowed, so the release stays on fat LTO with one codegen unit; a thin-LTO profile
does not match a fat-LTO build at all (150% of the functions unmatched), the gate catches it. BOLT changes the instruction
count by 0.2% (cycles per request moved between -9% and +8% under load and are inconclusive), below the 2% that pays for its
minutes, so a tag does not run it unless the repository variable `RELEASE_BOLT` is `1` or the manual run asks for it. Training
is not a cost worth parallelising: all 24 lanes replay in about 16 seconds, so the lane matrix of the first idea was not built; the cost
was the instrumented build, and that is what the cache removes. The `unwind` and `abort` flavours and the `aarch64` binary
already run in parallel jobs, each with its own profile (the symbol sets of `unwind` and `abort` differ in their control-flow
hashes, so they are not shared). The gate also refused a thin-LTO profile on a fat build, which is the proof that sharing
across configurations cannot happen silently.

The profile-use build in phases (`cargo build --timings`, 8 CPUs, `-j3`): the registry dependencies, the C builds of OpenSSL, jemalloc and
aws-lc among them, finish at 195 s; the `oxiroute` library then compiles for 115 s and the final `oxiroute` binary spends 237 s in
one-threaded fat-LTO codegen (548 s together). Rebuilding with every workspace source touched and the dependency objects of the
previous build in place takes 350 s (55 units compiled, 140 of 131931 functions unmatched): the dependency objects are 198 s, 36%, of
the build and 0.9 GB of cache. Fat LTO with 16 codegen units compiles the library in 54 s and the final crate in 302 s, 548 s together
again, with 0.3% to 1.8% more instructions per request than one codegen unit (a profile of one codegen-unit count does not match a
build of the other: 26% unmatched), so it is not used. `-Cprofile-use` reaches the dependencies through `RUSTFLAGS` (stable cargo has no
per-package rustflags) and their functions are in the profile, so their objects are a function of the profile bytes and are cached
under the profile key (below), never across profiles.

Critical path of a tag with the profile and the dependency objects in the cache, estimated on hosted runners (local x 2):

| Stage | Hosted est. | Note |
| --- | ---: | --- |
| `changes` | 1 min | the package-script selftest is its own job |
| `lookup` (profile) | 1 min | restore by exact key or newest of the version |
| container setup (image, packages, toolchain cache) | 3 min | unchanged |
| profile-use build: workspace, one-thread fat-LTO codegen | 11.7 min | 350 s local, was 548 s (18 min) |
| checks, smoke, tarball, upload | 2 min | |
| packages (assemble, then the slowest image) | 8 min | unchanged; all images already run in parallel |
| `publish` | 2 min | |
| Total | about 29 min | was about 35 with the profile only, more than an hour before |

The 25 minutes are not reached; the floor is the profile-use build of the workspace, 11.7 minutes: the `oxiroute` library (4 min) and the
one-threaded fat-LTO codegen of the final crate (8 min) change with every release, depend on the profile and cannot be cached or
split, and thin LTO, which would save 235 s local, costs up to 3.7% instructions per request. The remaining time is the fixed 3 minutes of
container setup and the 8 minutes of package checks (assemble job, then the slowest of 8 image jobs, each downloading its distribution);
no measurement of those jobs exists here, so they are unchanged. The x86_64 `unwind`, `abort` and `aarch64` binaries already build in separate
parallel jobs; attestation of the tarball (about half a minute) is the only step that could still move off the path. A miss of the
profile adds the instrumented build (26 min) once; a miss of the dependency objects adds 6.6 minutes. A dry run (`PGO=0 BOLT=0`) links without LTO
and with 16 codegen units; with the cached dependency objects it is about 6 minutes of build plus the 8 minutes of packages.

Caching: the toolchain and the cargo registry of the build container are cached with `actions/cache` keyed by `Cargo.lock`
(one entry per architecture), and so are the dependency objects of a plain dry-run build (`.package-cache/plain-work`). The
profiles are cache entries too (above). The dependency objects of the profile-use build (`.package-cache/pgo-work`, about 1 GB per leg) are cached too, under
`use-bolt0-PROFILEKEY` (exact key only: with `-Cprofile-use` each object is a function of the profile bytes, and cargo does not
notice a changed profile file, so a restore-key or a key that ignores the profile could return code compiled against another
profile). The refresh job `warm` builds once with each new profile to fill them, the tag build restores the entry of the profile it
uses. `sccache` hashes the command line, not the profile data, and is not used; `ccache` for the C builds alone was not measured (they
are part of the 198 s above). `Swatinem/rust-cache` serves the cargo builds that run on the runner itself (the BOLT tools and the workspace tests) and writes its
entry on `main` only. `CARGO_INCREMENTAL=0` is set in every release job. The instrumented build cannot reuse the objects of the
profile-use build: its `RUSTFLAGS` differ (`-Cprofile-generate`), which cargo fingerprints. Per-job timeouts bound a stuck
runner: 10 minutes for `changes` and `lookup`, 20 for assembling a package, 30 for one image, 90 for training, 120 for a binary or the tests.

### Test matrix

Each package is installed in fresh containers of the same architecture, one workflow job per image: the `.deb` on Debian 12
and 13 and Ubuntu 22.04 and 24.04; the `el9` `.rpm` on AlmaLinux 9, Rocky Linux 9 and AlmaLinux 10; the Amazon Linux 2023
`.rpm` on Amazon Linux 2023. The images are the unmodified distribution images, except that the test lets the package manager
install documentation (the container images drop it). Each run checks the file list against `packaging/arch`, the conffile marking, the user and state
directories, that nothing is enabled or started, `oxiroute version`, `oxiroute config check` of the shipped
configuration as the `oxiroute` user, `systemd-analyze verify`, the shipped configuration serving `/ready`, one HTTP/1 request
relayed by a proxy to a local origin (two `oxiroute` processes), a reinstall that keeps an edited configuration, the
upgrade from the previous release package when the previous release has one, `remove`, and `purge` (deb).

Results of the local run before the first release (x86_64, a plain `PGO=0` binary built on Debian 12, which needs glibc 2.34
at most; all eight images passed every check above except the upgrade):

| Package | Images |
| --- | --- |
| `.deb` | Debian 12, Debian 13, Ubuntu 22.04, Ubuntu 24.04: pass |
| `el9` `.rpm` | AlmaLinux 9, Rocky Linux 9, AlmaLinux 10: pass |
| `amzn2023` `.rpm` | Amazon Linux 2023: pass |

Open limits: the aarch64 packages, the container build on `almalinux:9`, the profile-guided and BOLT binaries and the
upgrade test (no earlier release carries a package) have only run in the release workflow, not locally. The unit uses
directives of systemd 254 (`DelegateSubgroup=`) and 257 (`ProtectControlGroups=private`): Debian 12 (systemd 252),
Ubuntu 22.04 (249) and Ubuntu 24.04 (255) ignore what they do not know with a warning, and `systemd-analyze verify` still
accepts the unit there.

## Binary Package

`packaging/arch/bin/` is the `oxiroute-bin` recipe: it installs the release tarball above and the same service,
sysusers, tmpfiles, environment, KDL, Lua and `.install` files as the source recipe (byte-identical copies, enforced by
`packaging/arch/verify-checksums.sh`). The tarball checksum exists only after the release workflow has run, so after
the publish job has attached the tarballs, run `packaging/arch/pin-bin-release.sh VERSION` and commit the result.
`verify-checksums.sh` runs in CI and in `scripts/verify/release/version.sh`; it fails on a stale local-file checksum or a
PKGBUILD/`.SRCINFO` disagreement. The same `packaging/arch/oxiroute.{service,sysusers,tmpfiles,modules,env,kdl,lua}`
files are the installed layout of the Debian and RPM packages too ([Release Assets And Packages](#release-assets-and-packages)).

## Release workflow

`.github/workflows/release.yml` runs on version tags, verifies Rust 1.97.1 metadata, version alignment,
the deterministic archive, the Arch checksum, and a source provenance manifest, and calls the reusable workflows
`release-binary.yml` (one binary of an architecture and flavour) and `release-packages.yml` (the packages of one
architecture); the binary workflow calls `release-profile.yml` (the cached PGO profile of an architecture and flavour), which
`profile-refresh.yml` also runs every night; every action in them is pinned by commit SHA. The tag workflow does not run a full
fuzz campaign. It publishes GitHub release assets only for a tag push; a manual run verifies a supplied tag or ref without
publishing, unless `publish` is set: then the archive job first checks that the rebuilt archive and
`SOURCE-PROVENANCE.json` are byte-identical to the assets already on the release of that tag (it never uploads them
again), and the publish job attaches the tarballs, packages, build records and the new `SHA256SUMS` to that release.
That is how the assets of a tag whose tag-push run failed after the source assets were published are added without moving
the tag or touching the source archive. A checksum mismatch is a deliberate release blocker, not a fallback.

`.github/workflows/audit.yml` runs pinned cargo-deny advisory, ban, license, and source checks plus
`cargo audit -D warnings`, all committed lockfile checks, and high-severity audits for the UI and
Remotion dependency roots. `deny.toml` rejects unknown registries and Git sources except the pinned
h3 repository and intentionally has no RustSec exceptions. The Pingora dependency paths are kept
auditable through vendored manifests.

`.github/workflows/pages.yml` uploads the `website/` directory using the official Pages artifact and
deployment actions. It runs on pushes to `main` and can be started manually. Repository Pages settings
must use **GitHub Actions** as the source; this repository change cannot alter organization settings.

The site is static and has no runtime dependency on the daemon. Keep source links pointed at the
repository's versioned docs so the published site remains navigable even when a feature is unavailable.
