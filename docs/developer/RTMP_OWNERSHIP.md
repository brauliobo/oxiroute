# RTMP Ownership

`oxiroute-rtmp` owns the RTMP protocol and runtime: sessions, stream identity, fanout, media,
recording, relay, auto-push, callbacks, and runtime control.

`oxiroute-import::nginx` owns nginx-RTMP source compatibility: the 117-key directive registry,
contexts, arities, value grammars, compatibility forms and statuses, reports, validation, parsing,
include expansion, provenance, diagnostics, and canonical lowering. The importer uses its existing
bounded byte parser; the former standalone RTMP parser was intentionally removed rather than moved.
The removed parser normalized an escaped space such as `/var/media\ files`, while the authoritative
importer preserves that unrecognized escape and its raw source span without a diagnostic. This
difference is intentional; import parsing does not emulate the removed parser.

The runtime crate does not depend on import syntax, and the importer does not depend on the RTMP
runtime. Canonical RTMP configuration remains the boundary between import lowering and runtime
construction.

Value-only RTMP plan construction is isolated in `oxiroute-rtmp::composition`. The
`scripts/verify-rtmp-plan-purity.mjs` gate parses that module's direct Rust import/module surface and
the RTMP crate's Cargo dependency surface against explicit allowlists, rejecting newly introduced
acquisition-capable dependencies by default. The server's `rtmp_value_mapping` module similarly owns
pure canonical field conversion; it must not resolve DNS, read files, open stores/logs, or construct
runtime workers. Static allowlisting is an architectural tripwire, not a proof that an allowlisted
owner type remains pure: review of changes to allowlisted constructors and validators remains a trust
boundary. Production performs canonical RTMP conversion once through the crate-private opaque
translator, then materializes runtime objects from those plans in the prior acquisition order; only
acquisition inputs such as access-log configuration remain canonical-side.
