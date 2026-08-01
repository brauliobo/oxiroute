# OxiRoute 0.2.1

OxiRoute 0.2.1 adds the first integrated Squid-style explicit HTTP/1 forward-proxy slice.

## Highlights

- Import a strict direct-forward Squid subset with `oxiroute import squid` or a `squid_server`
  native source reference. Native activation requires `externalize_cache = true` when refresh rules
  would be discarded by the direct, non-caching runtime.
- Serve absolute-form HTTP and CONNECT through the daemon's `forward_http1` listener.
- Enforce ordered access rules, bounded TTL caching for externally validated Basic credentials or
  Bearer authentication, explicit Basic username case policy, destination policy, bounded DNS, header privacy, and
  connection/body/header/time limits.
- Preserve bounded CONNECT byte, idle, lifetime, half-close, and shutdown behavior while using an
  unsplit duplex relay that flushes pending tunnel data correctly and accounts only delivered bytes.
- Forward early origin responses without waiting for an unfinished upload and close that downstream
  connection; enforce body limits while uploads are relayed, include DNS queueing in the connect
  deadline, and bound initial-header and keep-alive waits by service limits.
- Reject downstream TLS on forward-HTTP/1 listeners until its handshake can share the service's
  finite idle and lifetime admission bounds.
- Render imported candidates deterministically as KDL, Lua, HOCON, or UCI. Lua reserved-word field
  names now use bracketed keys and round-trip correctly.
- Correct HAProxy direct import for the live hostrouter shape: page-only stats with localhost
  administration, `unix@` listener mode, case-insensitive exact Host routing with a fixed `503`
  fallback, reusable HTTP `leastconn`, bare final redispatch, and exact health timeout preservation.
- Keep RTMP media queues draining while completed segments are synchronized and published on a
  dedicated finalizer, preventing large hourly recordings from failing with `queue_discontinuity`.
  Reconnects within `record_interval` continue the existing validated FLV segment.

## Compatibility

- Integrated forward proxying is HTTP/1 only. H2 and H3 remain unavailable in the daemon.
- Squid cache, refresh, peer, helper, adaptation, interception, TLS-bump, and delay-pool semantics
  remain unsupported. Cache/refresh rules are reported as externalized because forwarding is direct
  and non-caching.
- Existing nginx, HAProxy, RTMP, reverse HTTP, and L4 behavior remains covered by the full workspace
  suite.
- HAProxy stats authentication/other unsupported forms, redispatch interval arguments, broader
  ACLs, and server-selection policy still fail closed. Native log and process settings remain
  explicit deployment warnings and are not silently reproduced.
