# Security and Dependency Audit

Security checks are release gates, not advisory-only reporting. The CI policy is fail-closed: a
RustSec advisory, a lockfile mismatch, a high or critical JavaScript advisory, or an unapproved
release-artifact secret fails the relevant workflow.

## Rust Dependencies

`.github/workflows/audit.yml` runs cargo-deny `0.20.2` advisory, ban, license, and source checks
for the root graph, and cargo-audit `0.22.1` checks for every committed Cargo lockfile. `deny.toml`
deliberately keeps `ignore = []` and explicitly denies unmaintained dependencies. No RustSec ID may
be added to an ignore list to make a workflow pass.

OxiRoute and the vendored Pingora Rustls path use `rustls-pki-types` for PEM parsing. The
vendored Pingora daemon path uses maintained `daemonix`, and its derived debug implementation is
manual, so the former `daemonize`, `derivative`, and `rustls-pemfile` paths are no longer present.

The current checked-in root graph passes `cargo audit -D warnings` and the pinned cargo-deny
advisory, ban, license, and source checks. The clean result is achieved by dependency replacement,
not an advisory ignore or a baseline exception. A newly reported advisory is an additional
blocker and must be investigated rather than suppressed.

## Lockfiles

`scripts/verify-lockfiles.sh` runs locked Cargo metadata checks for the root workspace, fuzz
workspace, and benchmark load-generator workspace. It also runs frozen, lockfile-only pnpm checks
for both `ui` and `remotion`. The audit and release workflows invoke this script, so changing a
manifest without updating its committed lockfile fails before build or packaging work proceeds.

## Release Artifacts

`scripts/create-release-archive.sh` invokes `scripts/verify-release-archive.sh` before publishing an
archive. The verifier requires every committed lockfile and rejects build directories, dependency
install directories, secret-shaped paths, and high-signal credential content such as private-key
PEM blocks and common provider token formats. The known deterministic private-key fixtures are
allowlisted by exact path only; that allowlist is not a general secret exemption.

## JavaScript Dependencies

The audit workflow installs each pnpm root with `--frozen-lockfile` and runs
`pnpm audit --audit-level high` for both the dashboard UI and the Remotion media workspace. This
includes development dependencies because they are part of the checked-in build and release
toolchain.
