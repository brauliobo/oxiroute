# Developer Guide

OxiRoute is a Rust workspace with a Vue/Pug client and a static documentation site. The project
values small owning abstractions, bounded resource use, explicit status, and tests that demonstrate
failure behavior before a capability is advertised.

## Contribution Route

1. Read [the architecture](ARCHITECTURE.md) and identify the owning crate.
2. Find or add the smallest failing test at that abstraction.
3. Keep configuration, runtime planning, activation, and observability changes separate when possible.
4. Update the compatibility matrix and the relevant user/reference docs in the same change.
5. Run the focused test, then the repository gates in [TESTING.md](TESTING.md).
6. If the user-facing behavior changed, update the website content or its status data.

Do not describe a parser, build flag, or standalone crate as a daemon capability until the active
runtime path and failure cases are tested.

## Workspaces

| Crate/path | Owning concern |
| --- | --- |
| `oxiroute-server` | Binary, CLI, runtime planning, generation lifecycle, listeners, APIs, monitoring, topology, TLS |
| `oxiroute-config` | Canonical typed model, defaults, lexical policy, validation, restricted Lua |
| `oxiroute-config-source` | Format adapters, generic values, templates, native references, composition, rendering |
| `oxiroute-import` | Native parsers, semantic reports, provenance, diagnostics, canonical lowering |
| `oxiroute-forward-proxy` | Explicit forward-proxy target/auth/policy/tunnel primitives without socket I/O |
| `oxiroute-rtmp` | RTMP transport adapter, sessions, catalog, fanout, recording, FLV, relay, directive registry |
| `oxiroute-cache` | Bounded RFC-aware memory and descriptor-safe persistent cache core used by the reverse HTTP request path |
| `oxiroute-supervision*` | Platform-neutral protocol/state machines and Linux master/worker transport |
| `ui` | Vue API contract, runtime observatory, topology, recording panel, configuration editors |
| `website` | Public static communication surface |
| `remotion` | Documentation media source and repeatable dashboard recordings |

## Before You Edit

- Check the worktree; unrelated user changes may already be present.
- Search the existing tests and normative docs before adding a new abstraction.
- For existing product symbols, run GitNexus impact analysis before editing and inspect direct callers.
- Prefer correcting the owner of an invalid state over adding a call-site fallback.
- Avoid broad compatibility shims unless a persisted or shipped contract requires one.

## Key Contracts

- [Configuration specification](../CONFIG_SPEC.md)
- [API/UI specification](../API_UI_SPEC.md)
- [Operations contract](../OPERATIONS.md)
- [RTMP specification](../RTMP_SPEC.md)
- [Import specification](../IMPORT_SPEC.md)
- [Compatibility matrix](../COMPATIBILITY.md)
- [Test strategy](../TEST_STRATEGY.md)
