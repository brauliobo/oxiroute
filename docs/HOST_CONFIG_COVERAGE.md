# Audited host configuration coverage

## Scope

This ledger tracks the effective configurations read from `hostrouter.lan` and `phoenix.lan` on
2026-07-22, sanitized complete captures from `whitebeast`, `hostrouter`, and `phoenix` on
2026-07-26/27 with a whitebeast HAProxy recapture on 2026-07-31, and HAProxy-only captures from
`chicopc` and `back1` on 2026-07-28. Credentials and
certificate contents are intentionally excluded. A row is `covered`
only when canonical configuration, runtime behavior, failure handling, tests, and native lowering
exist. Manual approximation does not count as native compatibility.

`coverage/host-cases.json` is the authoritative machine-readable status and gate ledger. Only the
fixture-to-case mappings marked `live_origin_hashed_read_only_captured` in its `audit.fixtures`
section count as live-host evidence. This status records capture properties, not a cryptographic
signer authentication claim. Metadata records the exact read-only origin hash commands, direct
nginx/HAProxy origin hashes, the no-raw-byte sanitizer process, every distinct checked-in
post-sanitization file hash, and overlay inventory. Other
checked-in fixtures are synthetic implementation probes: they can test parsers,
lowerers, and failure behavior, but MUST NOT be described as evidence that an audited host case is
covered.

Status values are `covered`, `partial`, `missing`, `external`, and `inactive`.

## Cross-cutting import cases

| ID | Source behavior | Status | Required iteration |
| --- | --- | --- | --- |
| IMP-01 | nginx source files, deterministic includes, spans, and diagnostics | partial | Frontend implemented; canonical/runtime/native-lowering gates remain. |
| IMP-02 | nginx HTTP inheritance and virtual-server lowering | partial | The fragment and complete-root APIs can finalize a strict subset; broader semantics, audited candidates, and daemon integration remain blocked. |
| IMP-03 | nginx-RTMP include resolution, inheritance, and plan lowering | partial | Listener/application, chunk/log suppression, recording, and static push lowering can finalize; broad directive and audited gates remain. |
| IMP-04 | HAProxy ordered `-f` roots and directory expansion | partial | Ordered immutable loading is implemented and feeds finalized live-host candidates; broader native source forms remain outside the strict subset. |
| IMP-05 | HAProxy defaults/frontend/backend/listen resolution | partial | HTTP/TCP lowering, deterministic preprocessing, capacity, exact health/retry/timeout/reuse policy, case-insensitive Host routing, fixed fallback, and dedicated stats-page lowering finalize the live hostrouter shape; broader ACL, stats, and server policy remains blocked. |
| IMP-06 | Stable blocking diagnostics and provenance in canonical candidates | partial | Canonical provenance plus typed deployment, activation, secret-overlay, environment-fingerprint, and inactive-source records exist. |

## `hostrouter.lan` nginx cases

| ID | Effective behavior | Status | Notes |
| --- | --- | --- | --- |
| HN-01 | 33 HTTP virtual servers sharing IPv4/IPv6 ports 80 and 443 | partial | Host routing exists; per-vhost policy lowering does not. |
| HN-02 | Multiple exact hosts and default-server fallback | partial | Runtime and strict fragment lowering exist; the complete audited candidate remains blocked. |
| HN-03 | Leading-dot and leading-wildcard nginx names | partial | Canonical selectors and runtime longest-suffix precedence preserve nginx semantics; native lowering remains. |
| HN-04 | Nine file-backed certificate lineages selected by SNI on port 443 | partial | Canonical/runtime exact and wildcard DNS SNI selection, declared DNS/IP SAN subset verification, mapped-IP canonicalization, and strict TLS lowering exist; the audited multi-lineage candidate remains blocked. |
| HN-05 | Certbot lineage renewal and zero-downtime activation | partial | Watching and activation are implemented; Certbot lineage semantics and the audited candidate are not lowered. |
| HN-06 | Listener-wide TLS 1.2/1.3 and mixed H1/H2 ALPN | partial | Runtime and a strict nginx TLS subset exist; the audited candidate remains blocked. |
| HN-07 | Certbot cipher, DH, session-cache, and ticket policy | partial | OxiRoute uses a fixed secure policy and disables resumption. |
| HN-08 | Fixed-IP HTTP proxy destinations | partial | Runtime and strict fragment lowering exist; the audited service remains blocked. |
| HN-09 | DNS-named `.lan` proxy destinations | partial | Canonical/runtime and strict fragment lowering exist; authoritative audited gates remain incomplete. |
| HN-10 | Unix-socket upstream to HAProxy | partial | Canonical/runtime and strict fragment lowering exist on Unix; authoritative audited gates remain incomplete. |
| HN-11 | HTTPS origins with nginx verification/SNI defaults | partial | Static DNS authorities lower with verified SNI and system trust; IP-only ambiguity requires a uniquely matched explicit TLS overlay. The live-origin hashed host tree remains non-finalizable because unrelated overlays and policy are unsatisfied. |
| HN-12 | Variable destination using `$scheme` | partial | Exact `$scheme://<static-authority>` destinations specialize to HTTP or verified HTTPS from listener TLS state; arbitrary request-derived destinations remain blocked. |
| HN-13 | WebSocket and HMR upgrades | partial | Canonical validation admits only the standard managed nginx header pair and Pingora owns the wire upgrade; native lowering remains. |
| HN-14 | `proxy_set_header` literals and bounded nginx variables | partial | Canonical/runtime mutations and a strict lowering subset exist; audited forms remain unverified. |
| HN-15 | Redirects and explicit status responses | partial | Canonical/runtime actions and strict lowering exist; audited routes remain unverified. |
| HN-16 | Public static document root and index handling | partial | Descriptor-pinned root/alias mapping, ordered try-files, indexes, autoindex, MIME, headers, ETag on/off, errors, and ranges run on wire; the audited alias/try-files/ETag-off shape lowers, while broader nginx location reselection remains blocked. |
| HN-17 | Header-based bearer and file-backed Basic authorization | partial | Secret-file bearer and mixed bcrypt/APR1 htpasswd policies are integrated; redacted inline nginx authorization lowers only with one uniquely matched secret-file overlay. Unsupported hashes fail preparation. |
| HN-18 | Cookie path/attribute rewriting | partial | Cookie Path, Secure, HttpOnly, and SameSite rewrites run on wire; native attribute lowering remains. |
| HN-19 | Per-vhost body limits and separate connect/read/write timeouts | partial | Route-local limits and independent deadlines lower bare-second, suffixed, and ordered composite nginx times and run on wire, but the authenticated complete root remains non-finalizable while unrelated security overlays and native policy are unsatisfied. |
| HN-20 | Mixed buffered and unbuffered proxy paths | partial | Request buffering reads the bounded body before upstream connection and native lowering retains nginx's default; response buffering still fails startup rather than being ignored. |
| HN-21 | Gzip type/level policy | partial | Gzip-only streaming negotiation and native lowering cover effective on/off, level, concrete types, 20-byte minimum, HTTP/1.1 minimum, nginx's eligible status set, `gzip_proxied off`, and vary behavior even when request policy suppresses compression; all participating non-shadowed virtual servers on a bind must agree. The audited root remains blocked only by unrelated native policies and overlays. |
| HN-22 | Custom access/error log formats | partial | HTTP file logging emits fixed redacted JSONL; arbitrary source formats, separate error logs, and native lowering remain unsupported. |
| HN-23 | Declared but unapplied nginx rate/connection zones | partial | Parser decisions exist, but the complete native-lowering gate is not met. |

## `hostrouter.lan` HAProxy cases

| ID | Effective behavior | Status | Notes |
| --- | --- | --- | --- |
| HH-01 | Unix frontend socket with explicit mode | covered | The live `unix@` bind lowers with its explicit mode, reserves and registers that mode on Unix, and participates in the finalized audited candidate. A live mode change is saved as explicitly restart-required without mutating the active socket. |
| HH-02 | Exact Host ACL and ordered `use_backend` | covered | The live `hdr(host) -i` rule lowers to ASCII case-insensitive exact authority matching without port widening; unmatched requests reach an explicit final `503`. |
| HH-03 | `leastconn` across ten backend servers | covered | Reusable HTTP least-connections uses physical-connection work accounting and deterministic ties across the complete live endpoint set. |
| HH-04 | DNS-named server identities | covered | All ten live DNS identities remain canonical and unresolved during import, then use bounded runtime resolution. |
| HH-05 | HTTP GET health check, exact 200, interval/rise/fall | covered | Ordered health defaults lower with healthy startup and preserve the live interval as an equal exact timeout when `timeout check` is absent. Broader health methods/forms remain blocked outside this case. |
| HH-06 | `retries 3` and `redispatch` | covered | Bare redispatch lowers to delayed same-server retries with only the final retry redispatched immediately; redispatch interval forms still fail closed. |
| HH-07 | Client/connect/server/request/keepalive timeout classes | covered | Listener, route-local, pool, and health timeout scopes are independently preserved for the live candidate. |
| HH-08 | `forwardfor except` | covered | The audited loopback source exception lowers to the canonical source-CIDR policy and runs on wire. |
| HH-09 | Frontend and process-wide `maxconn` | covered | Aggregate, listener, and server caps retain and enforce their distinct scopes. |
| HH-10 | HAProxy stats page and conditional administration | covered | The dedicated live page lowers with implicit URI activation, refresh, frontend admission/timeouts, and transport-plus-authority loopback administration. It is page-only and creates no routes beyond its configured prefix; response rules fail closed, and auth/broader stats/Prometheus forms remain unsupported outside this case. |
| HH-11 | Syslog and HAProxy HTTP log policy | external | Import emits deployment warnings. OxiRoute does not reproduce HAProxy syslog destinations or HTTP log format; operators must supply an explicit deployment logging policy. |
| HH-12 | user/chroot/daemon process settings | external | Deployment unit or container owns these settings. |

## `phoenix.lan` nginx cases

| ID | Effective behavior | Status | Notes |
| --- | --- | --- | --- |
| PN-01 | Port-80 static root, index files, ETag policy, and custom 50x page | partial | Descriptor-pinned static indexes, ETag validators, and custom error documents run on wire; broader nginx location reselection remains blocked. |
| PN-02 | nginx worker/sendfile/keepalive controls | external | Some limits have non-equivalent runtime knobs. |
| PR-01 | RTMP listener on port 1935 | partial | Canonical/runtime and strict native lowering exist; the authoritative audited native-lowering gate remains incomplete. |
| PR-02 | Outbound RTMP chunk size 4096 | partial | Canonical policy is compiled into `ServerSessionConfig`, verified on wire, and lowered natively; authoritative fixture evidence remains incomplete. |
| PR-03 | Configured `live` application boundary and live publishing | covered | The audited Phoenix root finalizes with its explicit operational overlays, composes with the host HAProxy candidate, and preserves the exact live application boundary. |
| PR-04 | Live playback, late join, and bounded fanout | partial | Application-scoped canonical subscriber/message/byte limits drive runtime fanout; authoritative native lowering remains incomplete. |
| PR-05 | Automatic FLV recording of all tracks | covered | The audited Phoenix root lowers continuous legacy AVC/AAC recording, and process restart plus publisher reconnect creates a new finalized segment without changing prior bytes; unsupported enhanced codecs remain fail-closed. |
| PR-06 | Safe recording path, unique naming, interval rotation, suffix formatting | covered | The audited Phoenix root lowers its recording path, hourly rotation, Bahia timezone, native suffix, and collision policy; restart/reconnect and FLV-in-`.mp4` behavior are verified end to end. |
| PR-07 | RTMP push relay to loopback port 1936 | partial | The typed client relay retries the initially absent port, recovers, bootstraps media, isolates bounded backpressure, exposes redacted counters, and lowers static `$name` targets. |
| PR-08 | RTMP access-log suppression | partial | Explicit disabled policy runs without session access events and lowers natively; file RTMP access logging remains unsupported. |

## Inference-node HAProxy cases

`phoenix.lan`, `chicopc.lan`, and `back1.lan` have the same captured HAProxy source hash. Explicit
preprocessing selects `NODE_IP=10.0.0.11` without `GPU1` on Phoenix, `NODE_IP=10.0.0.15` with
`GPU1` on Chicopc, and `NODE_IP=10.0.0.7` with `GPU1` on Back1.

| ID | Host | Effective behavior | Status | Notes |
| --- | --- | --- | --- | --- |
| PI-01 | `phoenix.lan` | Three HTTP frontends, case-insensitive exact health responses, `balance first`, one GPU worker per pool, and native default retries | covered | Live-origin hash, explicit environment fingerprint, canonical lowering, and runtime path precedence are tested. |
| CI-01 | `chicopc.lan` | Same topology with two GPU workers per pool | covered | Live-origin hash, explicit environment fingerprint, canonical lowering, and runtime path precedence are tested. |
| BI-01 | `back1.lan` | Same topology with two GPU workers per pool | covered | Live-origin hash, explicit environment fingerprint, canonical lowering, and runtime path precedence are tested. |

## Iteration progress

| Iteration | Scope | Status |
| --- | --- | --- |
| A | Audit ledger and sanitized acceptance inventory | completed |
| B | Multi-certificate SNI routing on one TLS listener | completed |
| C | Certbot lineage snapshots and zero-downtime reconciliation | completed |
| D | Endpoint identity model, DNS/Unix endpoints, and least connections | completed |
| E | HTTP route actions, headers, per-route policies, and static serving | completed |
| F | nginx and HAProxy source frontends and semantic lowering | partial |
| G | RTMP application plan, playback/fanout, FLV recording, and push relay | partial |

## KDL deployment verification

On 2026-07-29, the active Phoenix, Chicopc, and Back1 deployments moved from materialized Lua to
KDL 2.0 roots that reference their existing native nginx and HAProxy files. Before each restart, the
new root and the prior Lua file were independently resolved to deterministic KDL and required to be
byte-identical.

| Host | KDL root SHA-256 | Resolved revision | Live gates |
| --- | --- | --- | --- |
| `phoenix.lan` | `7d1f12e09a349518e280d40053b7d21106d80aebe5c56829e212be43c86d074b` | `b27e9df312169ddb5fdd4e98c1ee127b6ad57d197e7bb6732a3222fe9ee15816` | Exact root and nginx 404 body hashes, three inference health responses, structured access-log growth, recorder ownership, and the established ten-publisher baseline passed. One extra pre-restart publisher was transient and did not return during the bounded soak. |
| `chicopc.lan` | `4f8dd2852d181daee732f243ff1e19c889b01a58d7b1cdc0f6cfbc7978b64490` | `d97fedd5ff51d3b082ac1b18c9ae673145b640f4118a948e1a780f610857ab76` | All three inference health responses passed after restart. |
| `back1.lan` | `6e263a653b953776fd4b4aa881403fa34771cfb2cbc094e530c494c428512907` | `7a9abd3f66873dd57d33527e4a83c7748a752540687f4bac0a72ce4b4e3cecbe` | All three inference health responses passed after restart. |

All three hosts run binary SHA-256
`b2de4949a07d35dcc413469d456a66928c67a12aa736a15e3c2d0390f8bc1c59`, keep OxiRoute enabled,
keep replaced native services disabled, and emitted no warning-level OxiRoute journal entries during
the migration soak.

### Phoenix config-watcher CPU deployment

On 2026-07-30, Phoenix upgraded to package SHA-256
`0d2fe314112fa3bc5e8d39b5bf0a2cbe41c2401837c860a9a0ddcebdb018c535`, containing binary SHA-256
`410d8d3583500452d506a04abd40f16b3180642d3a19ed25b0847f5233af8e69`. The release ignores
filesystem access notifications generated by periodic reads of the canonical configuration, removing
the read-event reconciliation feedback loop. The KDL root, Phoenix systemd drop-in, effective
`http:http` identity, writable paths, and resolved revision remained unchanged across the upgrade.

Before the upgrade, the process consumed 1 hour 2 minutes 51.030 seconds of CPU over 12 hours 33
minutes 13.432 seconds of wall time, or 8.3442% of one core, with a one-second preflight sample
assigning 9% to the configuration watcher. After the upgrade, a 330.218-second soak measured 0.7752%
process CPU and 0.0727% watcher CPU. Eleven consecutive watcher cycles remained approximately 30
seconds apart, with no return to the prior sustained load.

The exact port-80 root and nginx-compatible 404 body hashes, all three inference health responses,
native-service disabled state, listener ownership, storage ownership, and continuous recording growth
passed after restart. The immediate pre-restart RTMP connection count fluctuated from 12 to 11 and
back to 12; after restart, the documented ten-connection baseline remained stable throughout an
extended five-minute sample. Recordings grew in every interval, and the new service emitted no
warning-or-higher journal entries.

## Operational observations

- Every currently captured native configuration passed its product's read-only validator.
- `hostrouter.lan` nginx reports deprecated/redefined HTTP/2 listener options and a type-hash sizing
  warning.
- `phoenix.lan` nginx reports a type-hash sizing warning.
- No process was listening on `phoenix.lan` TCP port 1936 during the audit although RTMP push targets
  it.
- One nginx virtual host contains an inline bearer credential. The value is not retained here and
  must be represented by a secret reference before native lowering can be supported.
