# Releasing

This page records the current release communication path. It does not replace the package recipe or
the test strategy.

## Before A Release

- Run the full Rust and UI gates in [TESTING.md](TESTING.md).
- Re-check [COMPATIBILITY.md](../COMPATIBILITY.md) against the active runtime, not just crate code.
- Update the relevant user/reference pages and add release notes under `docs/`.
- Render the dashboard GIFs if UI labels or product states changed.
- Inspect generated archives and ensure secrets, `target/`, `node_modules/`, and benchmark artifacts
  are absent. `scripts/verify-release-archive.sh` enforces this list and checks the Git-tracked file
  list when called with `--compare-worktree`.
- Verify the Arch recipe and `.SRCINFO` only after the release asset exists.

## Version Sources

The workspace version is in the root `Cargo.toml`; the server exposes it through `version`. Release
notes live in `docs/RELEASE_NOTES_<version>.md`. The Arch recipe pins the release archive and checksum.
Keep all three aligned.

## Package Path

`scripts/create-release-archive.sh` creates a deterministic source archive from Git-tracked files,
excluding the Arch recipe and benchmark report artifacts. `scripts/verify-release-archive.sh` checks
the root prefix, required locks/license, forbidden artifact and secret-shaped paths, and optional
worktree file-list equality. `packaging/arch/build-local.sh` reuses those checks before invoking
`makepkg`; it still checks the recipe checksum. The package installs the daemon, management client,
importer, service metadata, examples, and documentation but does not enable or start systemd
automatically.

Before creating an archive, verify the version metadata and release notes:

```sh
./scripts/verify-release-version.sh 0.3.0
./scripts/create-release-archive.sh /tmp/oxiroute-0.3.0.tar.gz 0.3.0
./scripts/verify-release-archive.sh /tmp/oxiroute-0.3.0.tar.gz 0.3.0 --compare-worktree
```

Read [packaging/arch/README.md](../../packaging/arch/README.md) before changing service permissions,
recording roots, or package files.

## GitHub Pages

`.github/workflows/release.yml` runs on version tags, verifies Rust 1.87 metadata, version alignment,
the deterministic archive, the Arch checksum, and a source provenance manifest, then uploads and
attests the archive. It publishes GitHub release assets only for a tag; manual runs verify a supplied
tag or ref without publishing. A checksum mismatch is a deliberate release blocker, not a fallback.

`.github/workflows/audit.yml` runs pinned RustSec and cargo-deny checks plus a high-severity UI audit.
`deny.toml` rejects unknown registries and Git sources except the pinned h3 repository. Existing
advisories remain visible and fail the gate until their owning dependency is updated or the release is
explicitly held.

`.github/workflows/pages.yml` uploads the `website/` directory using the official Pages artifact and
deployment actions. It runs on pushes to `main` and can be started manually. Repository Pages settings
must use **GitHub Actions** as the source; this repository change cannot alter organization settings.

The site is static and has no runtime dependency on the daemon. Keep source links pointed at the
repository's versioned docs so the published site remains navigable even when a feature is unavailable.
