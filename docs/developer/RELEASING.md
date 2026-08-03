# Releasing

This page records the current release communication path. It does not replace the package recipe or
the test strategy.

## Before A Release

- Run the full Rust and UI gates in [TESTING.md](TESTING.md).
- Re-check [COMPATIBILITY.md](../COMPATIBILITY.md) against the active runtime, not just crate code.
- Update the relevant user/reference pages and add release notes under `docs/`.
- Render the dashboard GIFs if UI labels or product states changed.
- Inspect generated archives and ensure secrets, `target/`, `node_modules/`, and benchmark artifacts
  are absent.
- Verify the Arch recipe and `.SRCINFO` only after the release asset exists.

## Version Sources

The workspace version is in the root `Cargo.toml`; the server exposes it through `version`. Release
notes live in `docs/RELEASE_NOTES_<version>.md`. The Arch recipe pins the release archive and checksum.
Keep all three aligned.

## Package Path

`packaging/arch/build-local.sh` creates a deterministic source archive from Git-tracked files and
checks it against the recipe. The package installs the daemon, management client, importer, service
metadata, examples, and documentation but does not enable or start systemd automatically.

Read [packaging/arch/README.md](../../packaging/arch/README.md) before changing service permissions,
recording roots, or package files.

## GitHub Pages

`.github/workflows/pages.yml` uploads the `website/` directory using the official Pages artifact and
deployment actions. It runs on pushes to `main` and can be started manually. Repository Pages settings
must use **GitHub Actions** as the source; this repository change cannot alter organization settings.

The site is static and has no runtime dependency on the daemon. Keep source links pointed at the
repository's versioned docs so the published site remains navigable even when a feature is unavailable.
