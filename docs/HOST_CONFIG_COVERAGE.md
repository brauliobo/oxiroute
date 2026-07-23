# Audited host configuration coverage

## Scope

This ledger tracks the effective configurations read from `hostrouter.lan` and `phoenix.lan` on
2026-07-22. Credentials and certificate contents are intentionally excluded. A row is `covered`
only when canonical configuration, runtime behavior, failure handling, tests, and native lowering
exist. Manual approximation does not count as native compatibility.

`coverage/host-cases.json` is the authoritative machine-readable status and gate ledger. Only the
sanitized fixture-to-case mappings listed in its `audit.fixtures` section count as audited host
evidence. Other checked-in fixtures are synthetic implementation probes: they can test parsers,
lowerers, and failure behavior, but MUST NOT be described as evidence that an audited host case is
covered.

Status values are `covered`, `partial`, `missing`, `external`, and `inactive`.

## Cross-cutting import cases

| ID | Source behavior | Status | Required iteration |
| --- | --- | --- | --- |
| IMP-01 | nginx source files, deterministic includes, spans, and diagnostics | partial | Frontend implemented; canonical/runtime/native-lowering gates remain. |
| IMP-02 | nginx HTTP inheritance and virtual-server lowering | partial | Semantic resolution exists; all current HTTP candidates remain draft-only. |
| IMP-03 | nginx-RTMP include resolution, inheritance, and plan lowering | partial | A strict listener/application/recording subset can finalize; broad directive lowering and audited gates remain. |
| IMP-04 | HAProxy ordered `-f` roots and directory expansion | partial | Frontend implemented; full audited lowering remains. |
| IMP-05 | HAProxy defaults/frontend/backend/listen resolution | partial | Resolver implemented; audited host candidates remain blocked. |
| IMP-06 | Stable blocking diagnostics and provenance in canonical candidates | partial | Report primitives and candidate provenance exist; complete native lowering remains. |

## `hostrouter.lan` nginx cases

| ID | Effective behavior | Status | Notes |
| --- | --- | --- | --- |
| HN-01 | 33 HTTP virtual servers sharing IPv4/IPv6 ports 80 and 443 | partial | Host routing exists; per-vhost policy lowering does not. |
| HN-02 | Multiple exact hosts and default-server fallback | partial | Runtime exact/catch-all routes exist; native lowering does not. |
| HN-03 | Leading-dot and leading-wildcard nginx names | partial | Canonical wildcard matches exactly one label. |
| HN-04 | Nine file-backed certificate lineages selected by SNI on port 443 | partial | Canonical/runtime exact and wildcard SNI selection is wire-tested; native nginx lowering is absent. |
| HN-05 | Certbot lineage renewal and zero-downtime activation | partial | Strict lineage watching, reconciliation, monitoring, and zero-downtime activation are implemented; native nginx lowering is absent. |
| HN-06 | Listener-wide TLS 1.2/1.3 and mixed H1/H2 ALPN | partial | Runtime support exists; nginx lowering does not. |
| HN-07 | Certbot cipher, DH, session-cache, and ticket policy | partial | OxiRoute uses a fixed secure policy and disables resumption. |
| HN-08 | Fixed-IP HTTP proxy destinations | partial | Runtime support exists; nginx lowering does not. |
| HN-09 | DNS-named `.lan` proxy destinations | partial | Canonical/runtime DNS endpoints exist; nginx native lowering and the authoritative audited gates remain incomplete. |
| HN-10 | Unix-socket upstream to HAProxy | partial | Canonical/runtime Unix endpoints exist on Unix; nginx native lowering and the authoritative audited gates remain incomplete. |
| HN-11 | HTTPS origins with nginx verification/SNI defaults | missing | OxiRoute intentionally requires verified DNS SNI. |
| HN-12 | Variable destination using `$scheme` | missing | Request-derived destinations remain blocked. |
| HN-13 | WebSocket and HMR upgrades | partial | Runtime transport is covered; header lowering is absent. |
| HN-14 | `proxy_set_header` literals and bounded nginx variables | missing | Only Host preservation is implemented. |
| HN-15 | Redirects and explicit status responses | missing | Routes currently require an upstream pool. |
| HN-16 | Public static document root and index handling | missing | Management assets are not a public static server. |
| HN-17 | Header-based bearer authorization | missing | Secret-backed request policy is absent. |
| HN-18 | Cookie path/attribute rewriting | missing | No response-cookie policy exists. |
| HN-19 | Per-vhost body limits and separate connect/read/write timeouts | partial | One body limit and one I/O timeout exist per HTTP service. |
| HN-20 | Mixed buffered and unbuffered proxy paths | partial | Runtime streams; bounded nginx-style buffering is absent. |
| HN-21 | Gzip type/level policy | missing | Compression policy is absent. |
| HN-22 | Custom access/error log formats | missing | Structured access logging remains planned. |
| HN-23 | Declared but unapplied nginx rate/connection zones | partial | Parser decisions exist, but the complete native-lowering gate is not met. |

## `hostrouter.lan` HAProxy cases

| ID | Effective behavior | Status | Notes |
| --- | --- | --- | --- |
| HH-01 | Unix frontend socket with explicit mode | partial | Canonical/runtime and strict static-TCP lowering exist on Unix; the complete audited candidate remains blocked. |
| HH-02 | Exact Host ACL and ordered `use_backend` | partial | Exact routes exist; HAProxy first-match lowering does not. |
| HH-03 | `leastconn` across ten backend servers | partial | Runtime leases and strict static-TCP lowering exist; audited HTTP accounting remains unrepresentable. |
| HH-04 | DNS-named server identities | partial | Canonical/runtime and strict static-TCP lowering exist; the authoritative audited gates remain incomplete. |
| HH-05 | HTTP GET health check, exact 200, interval/rise/fall | partial | Runtime probe exists with different Host/startup semantics. |
| HH-06 | `retries 3` and `redispatch` | partial | Safe distinct-endpoint connect retries are narrower. |
| HH-07 | Client/connect/server/request/keepalive timeout classes | partial | Timeout scopes are not independently modeled. |
| HH-08 | `forwardfor except` | missing | Forwarded-header policy is absent. |
| HH-09 | Frontend and process-wide `maxconn` | partial | Per-listener admission exists; no aggregate process cap. |
| HH-10 | HAProxy stats page and conditional administration | missing | OxiRoute management is a different loopback-only contract. |
| HH-11 | Syslog and HAProxy HTTP log policy | missing | Structured logging remains planned. |
| HH-12 | user/chroot/daemon process settings | external | Deployment unit or container owns these settings. |

## `phoenix.lan` nginx cases

| ID | Effective behavior | Status | Notes |
| --- | --- | --- | --- |
| PN-01 | Port-80 static root, index files, and custom 50x page | missing | General static-file actions are absent. |
| PN-02 | nginx worker/sendfile/keepalive controls | external | Some limits have non-equivalent runtime knobs. |
| PR-01 | RTMP listener on port 1935 | partial | Canonical/runtime and strict native lowering exist; the authoritative audited native-lowering gate remains incomplete. |
| PR-02 | Outbound RTMP chunk size 4096 | partial | Current dependency default matches but is not compiled from config. |
| PR-03 | Configured `live` application boundary and live publishing | partial | Canonical/runtime and strict native lowering exist; the authoritative audited native-lowering gate remains incomplete. |
| PR-04 | Live playback, late join, and bounded fanout | missing | Listener playback/fanout exists, but the authoritative audited-case gates remain incomplete. |
| PR-05 | Automatic FLV recording of all tracks | missing | Canonical continuous legacy AVC/AAC recording and strict lowering exist, but authoritative audited-case gates remain incomplete and enhanced codecs are rejected. |
| PR-06 | Safe recording path, unique naming, interval rotation, suffix formatting | missing | Canonical/runtime path, storage, rotation, and strict lowering exist, but authoritative audited-case gates remain incomplete. |
| PR-07 | RTMP push relay to loopback port 1936 | missing | Relay worker is absent. |
| PR-08 | RTMP access-log suppression | partial | No RTMP access logger currently runs. |

## `phoenix.lan` dormant HAProxy cases

The valid HAProxy file is inactive. If activated, cases `HH-01` through `HH-12` apply with eight
DNS-named backend servers instead of ten. They remain migration blockers but are not active traffic.

## Iteration progress

| Iteration | Scope | Status |
| --- | --- | --- |
| A | Audit ledger and sanitized acceptance inventory | completed |
| B | Multi-certificate SNI routing on one TLS listener | completed |
| C | Certbot lineage snapshots and zero-downtime reconciliation | completed |
| D | Endpoint identity model, DNS/Unix endpoints, and least connections | completed |
| E | HTTP route actions, headers, per-route policies, and static serving | pending |
| F | nginx and HAProxy source frontends and semantic lowering | partial |
| G | RTMP application plan, playback/fanout, FLV recording, and push relay | partial |

## Operational observations

- Both effective nginx configurations and both HAProxy files passed native validation.
- `hostrouter.lan` nginx reports deprecated/redefined HTTP/2 listener options and a type-hash sizing
  warning.
- `phoenix.lan` nginx reports a type-hash sizing warning.
- No process was listening on `phoenix.lan` TCP port 1936 during the audit although RTMP push targets
  it.
- One nginx virtual host contains an inline bearer credential. The value is not retained here and
  must be represented by a secret reference before native lowering can be supported.
