# OxiRoute 0.5.0

OxiRoute 0.5.0 advances the pre-alpha runtime with broader HTTP/3 and cache behavior, more durable
control-plane state, stronger native-import evidence, and tighter supervision and RTMP lifecycle
boundaries. It also reorganizes large implementation modules behind their existing public facades so
future protocol work can remain isolated without changing the current configuration contract.

## Highlights

- Reuse eligible reverse and forward HTTP/3 upstream connections while retaining bounded request,
  cancellation, and cache transaction behavior.
- Complete bounded reverse and forward HTTP/3 cache lookup, collapsed fills, revalidation,
  stale-if-error handling, admission, purge, and observable cache outcomes.
- Expand forward proxy coverage across HTTP/1 absolute-form, CONNECT and CONNECT-UDP plus bounded
  HTTP/2 and HTTP/3 paths with explicit destination policy.
- Strengthen supervised master/worker replacement with authenticated typed descriptor transfer,
  validated wire envelopes, bounded observation state, and UDP/HTTP3 drain coverage.
- Add durable ACME state and polling boundaries, direct-file and Certbot reconciliation, and expanded
  managed-certificate operations and diagnostics.
- Harden RTMP parsing, relay, recording, media segmentation, auto-push, callback, and session cleanup
  while preserving the existing public RTMP surface for this release.
- Expand nginx, HAProxy, Apache, Squid, and Varnish import provenance, deterministic source graphs,
  report evidence, and fail-closed unsupported-semantics handling.
- Split configuration models, validation, rendering, import workflows, UI API decoding, and test
  support into ownership-focused modules without changing their facade contracts.
- Expand the Vue operations, certificate, configuration, provenance, audit, and event workspaces with
  responsive browser and request-ownership coverage.

## Operational Notes

- The release line remains pre-alpha. Existing configuration is validated before activation, and
  unsupported native semantics continue to fail closed.
- Packaged Linux installations include the supervised launcher; eligible listener topologies use the
  supervised master/worker path, while unsupported topologies retain the direct runtime.
- RTMP relay and callback destinations reject private or local addresses by default. Intentional local
  targets require a narrow explicit outbound policy.
- KDL remains the default packaged configuration. Restricted Lua compatibility remains available.

## Evidence Boundary

The repository includes locked Rust 1.97.1 workspace gates, UI unit/type/build/browser gates, bounded
fuzz-target checks, archive and dependency policy, protocol wire tests, importer report fixtures, and
supervision lifecycle tests. CA-staging issuance, long-running fuzz campaigns, broad external
interoperability, and production active-traffic replacement remain separate evidence gates.
