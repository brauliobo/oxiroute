# Releasing

This page records the current release communication path. It does not replace the package recipe or
the test strategy.

## Before A Release

- Run the full Rust and UI gates in [TESTING.md](TESTING.md).
- Run `./scripts/verify-control-plane-contract.sh` to verify the checked OpenAPI artifact is generated
  from the registry and DTO schemas without drift.
- Run `./scripts/verify-lockfiles.sh` so every Cargo and pnpm manifest is checked against its
  committed lockfile.
- Re-check [COMPATIBILITY.md](../COMPATIBILITY.md) against the active runtime, not just crate code.
- Update the relevant user/reference pages and add release notes under `docs/`.
- Render the dashboard GIFs if UI labels or product states changed.
- Inspect generated archives and ensure secrets, `target/`, `node_modules/`, and benchmark artifacts
  are absent. `scripts/verify-release-archive.sh` enforces this list and checks the Git-tracked file
  list when called with `--compare-worktree`.
- Verify the Arch recipe and `.SRCINFO` only after the release asset exists.
- Run `scripts/verify-rtmp-public-api.sh` only when proving the immutable RTMP Phase 0 fixture against
  the matching current 0.4.1 API. Its command semantics are exact equality between the current
  `oxiroute-rtmp` rustdoc graph and `docs/developer/fixtures/rtmp-public-api-phase0.snapshot`. It is
  expected to fail after the authorized 0.5 removals and is not a 0.5 acceptance gate. Do not
  regenerate the Phase 0 fixture. For 0.5 acceptance, require the exact classified generalized RTMP
  delta from `scripts/verify-public-api.sh` and the future pinned `cargo-public-api` semantic
  comparison described below.
- Run `scripts/verify-public-api-baseline-provenance.sh` to regenerate all-features rustdoc JSON from
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
  `scripts/verify-public-api-baseline-provenance.sh --adversarial-self-test` to prove mutations of
  either source fail digest verification. `--update` is reserved for intentional, reviewed baseline
  regeneration from that same authenticated commit; changing canonicalizer bytes requires separately
  reviewing and updating both hardcoded and documented digests.
- Run `scripts/verify-generation-blueprint-baseline.sh` to regenerate the comprehensive runtime-plan
  behavior fixture from exact commit `2d9c5fe66cd096d7a1d8e3bada8d5784b5f97f6c`. The gate authenticates
  its archived instrumentation and harness with hardcoded SHA-256 digests, compares schema-2
  normalized decisions, acquired service/TLS and pool outputs, exact topology, errors, and acquisition
  trace stop points byte-for-byte against the candidate, and removes its `/tmp/opencode` workspace on
  exit. The reviewed instrumentation digest is
  `ba9a2ad1252ffe96bb4d5a84a40ce226a2a7ae374feeb1241b0549d4feaa11df`; the reviewed harness digest
  is `99b173a270a21f24c5766e1611a854a226892ea794f40c4033406e7f29cc8cbc`, and the generated fixture
  digest is `b83eb1bb086d72b9eae8030aeb9ed1cd799d82fb415fcc2401e0f7bbabccabd3`.
  Run `scripts/verify-generation-blueprint-baseline.sh --adversarial-self-test` to prove mutations of
  either immutable input fail authentication.
- Run `scripts/verify-public-api.sh` for the fast candidate comparison. It uses the identical
  toolchain, target, features, and schema metadata and checks each generated delta against the
  classified `docs/developer/fixtures/*-public-api-0.5.delta` files. The canonicalizer derives
  first-party dependency aliases from each dependency rustdoc root and rejects unresolved private
  definition paths. Baseline and candidate inventories are comparable only when their schema fields
  match. Use `--update` only after every changed item is accounted for in the release notes.
- Before the `0.5` API boundary, also use pinned `cargo-public-api` 0.52.0 to diff the exact `v0.4.1`
  release source against the release candidate as an independent cross-check.

## Version Sources

The workspace version is in the root `Cargo.toml`; the server exposes it through `version`. Release
notes live in `docs/RELEASE_NOTES_<version>.md`. Notes written ahead of a release are named `docs/RELEASE_NOTES_<version>_DRAFT.md` and renamed when the
release is cut. The Arch recipe pins the release archive and checksum.
Keep all three aligned.

## Package Path

`scripts/create-release-archive.sh` creates a deterministic source archive from committed `HEAD`,
excluding the Arch recipe and benchmark report artifacts. `scripts/verify-release-archive.sh` checks
the root prefix, every committed lock/license input, forbidden artifact and secret-shaped paths,
private-key and high-signal credential content, and optional committed file-list equality.
`packaging/arch/build-local.sh` reuses those checks before invoking `makepkg`; it still checks the
recipe checksum. The package installs the daemon, management client, importer, service metadata,
examples, and documentation but does not enable or start systemd automatically.

Before creating an archive, verify the version metadata and release notes:

```sh
./scripts/verify-release-version.sh 6.3.0
./scripts/create-release-archive.sh /tmp/oxiroute-6.3.0.tar.gz 6.3.0
./scripts/verify-release-archive.sh /tmp/oxiroute-6.3.0.tar.gz 6.3.0 --compare-worktree
```

Read [packaging/arch/README.md](../../packaging/arch/README.md) before changing service permissions,
recording roots, or package files.

## Release Binaries And PGO

`.github/workflows/release.yml` has a second job, `binaries`, that runs after the archive job and builds the
`x86_64-unknown-linux-gnu` binaries (`oxiroute`, the `oxr` alias and `oxiroute-worker-launcher`) with
profile-guided optimization, which cuts user-space instructions per request by 14 to 27%
([PERF_6_3_BUILD_AND_ALLOCATOR.md](PERF_6_3_BUILD_AND_ALLOCATOR.md)). The job is a thin call of
`scripts/build-release-binaries.sh VERSION OUT_DIR`, which calls `benchmarks/scripts/pgo.sh`:

1. `pgo.sh train`: instrumented build (`-Cprofile-generate`), replay of ten R2 data-path lanes through the
   instrumented proxy with `benchmarks/r2-data-path/run.sh --train` (loopback only, no `perf`, so it runs on GitHub
   runners), `llvm-profdata merge` into one profile.
2. `pgo.sh build`: release build with `-Cprofile-use` and the same cargo command. It parses the compiler's
   `-pgo-warn-missing-function` diagnostics and fails when more than 5% of the profile's functions were not matched,
   so a profile the compiler ignored cannot be published as a PGO build.
   Every build of the script (instrumented, profile-use, plain) appends the same `--remap-path-prefix` flags for the
   checkout, the cargo home and `PGO_WORK`, so the binary embeds no runner or home-directory path and the profile still
   matches. `scripts/build-release-binaries.sh` then runs `scripts/verify-binary-paths.sh` and fails when a binary
   contains `/home/`, `/runner/work`, the checkout or the cargo home. The vendored OpenSSL records its own build
   directory, which no rustc flag can remap, so `PGO_WORK` must lie outside `/home` (the workflow uses `/tmp`).
3. `pgo.sh smoke`: `oxiroute config check` of a generated configuration and two lanes of proxied requests through the
   final binary; any load error fails the job.

The tarball `oxiroute-VERSION-TARGET.tar.gz` (layout of the Arch package: `bin/oxiroute`, `bin/oxr`,
`lib/oxiroute/oxiroute-worker-launcher`, `LICENSE`, `BUILD-INFO.json`), its `.sha256` and `.build.json` (version,
commit, `rustc --version`, `pgo`, SHA-256 of the profile) are uploaded as workflow artifacts, attested, and attached to
the tag's release; a line stating that the binaries are PGO builds is appended to the release notes. A training,
profile or smoke failure fails the job: there is no silent plain build. A manual run can start with the `pgo` input set
to `false`, which publishes a plain build and says so in the notes line.

Requirements: the toolchain that trains must be the toolchain that builds. The profile is keyed by symbol names, and
rustc hashes its own version and the unit configuration into every symbol, so the job installs the pinned Rust 1.97.1
with the `llvm-tools` component (its `llvm-profdata` reads that compiler's raw format) and nothing else compiles
between training and the final build. The profile is generated in the job and never committed or archived: the source
archive, its checksum and `scripts/create-release-archive.sh` are unaffected, and the Arch recipe keeps building from
source without a profile.

To regenerate locally with the same compiler:

```sh
export PATH=$HOME/.rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu/bin:$PATH   # rustup component add llvm-tools
PGO_WORK=/dev/shm/oxiroute-pgo ./scripts/build-release-binaries.sh 6.3.0 /tmp/oxiroute-release
PGO=0 ./scripts/build-release-binaries.sh 6.3.0 /tmp/oxiroute-plain               # plain build of the same command
```

`PGO_LANES` and `PGO_SCALE` shorten the training for a local check of the flow; a profile from a shortened or different
workload still builds and passes the gate but gives less than the 14 to 27%. Release binaries are linked against the
glibc of the runner image (Ubuntu 24.04); older distributions need the Arch-style source build.

## Binary Package

`packaging/arch/bin/` is the `oxiroute-bin` recipe: it installs the release tarball above and the same service,
sysusers, tmpfiles, environment, KDL, Lua and `.install` files as the source recipe (byte-identical copies, enforced by
`packaging/arch/verify-checksums.sh`). The tarball checksum exists only after the release workflow has run, so after the
binaries job publishes, run `packaging/arch/pin-bin-release.sh VERSION` and commit the result. `verify-checksums.sh`
runs in CI and in `scripts/verify-release-version.sh`; it fails on a stale local-file checksum or a PKGBUILD/`.SRCINFO`
disagreement.

## GitHub Pages

`.github/workflows/release.yml` runs on version tags, verifies Rust 1.97.1 metadata, version alignment,
the deterministic archive, the Arch checksum, and a source provenance manifest, then uploads and
attests the archive. The tag workflow does not run a full fuzz campaign.
It publishes GitHub release assets only for a tag; manual runs verify a supplied tag or ref without
publishing. A checksum mismatch is a deliberate release blocker, not a fallback.

`.github/workflows/audit.yml` runs pinned cargo-deny advisory, ban, license, and source checks plus
`cargo audit -D warnings`, all committed lockfile checks, and high-severity audits for the UI and
Remotion dependency roots. `deny.toml` rejects unknown registries and Git sources except the pinned
h3 repository and intentionally has no RustSec exceptions. The Pingora dependency paths are kept
auditable through vendored manifests and their clean replacement record is maintained in
[SECURITY_AUDIT.md](SECURITY_AUDIT.md).

`.github/workflows/pages.yml` uploads the `website/` directory using the official Pages artifact and
deployment actions. It runs on pushes to `main` and can be started manually. Repository Pages settings
must use **GitHub Actions** as the source; this repository change cannot alter organization settings.

The site is static and has no runtime dependency on the daemon. Keep source links pointed at the
repository's versioned docs so the published site remains navigable even when a feature is unavailable.
