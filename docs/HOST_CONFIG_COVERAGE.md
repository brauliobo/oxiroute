# Audited host configuration coverage

## Scope

This ledger tracks the effective configurations read from `hostrouter.lan` and `phoenix.lan` on
2026-07-22, sanitized complete captures from `whitebeast`, `hostrouter`, and `phoenix` on
2026-07-26/27, and HAProxy-only captures from `chicopc` and `back1` on 2026-07-28. Credentials and
certificate contents are intentionally excluded. A row is `covered`
only when canonical configuration, runtime behavior, failure handling, tests, and native lowering
exist. Manual approximation does not count as native compatibility.

`coverage/host-cases.json` is the authoritative machine-readable status and gate ledger. Only the
fixture-to-case mappings marked `live_origin_hashed_read_only_captured` in its `audit.fixtures`
section count as live-host evidence. This status records capture properties, not a cryptographic
signer authentication claim. Metadata records the exact read-only origin hash commands, direct
2026-07-26 nginx/HAProxy origin hashes, the no-raw-byte sanitizer process, every distinct checked-in
post-sanitization file hash, and overlay inventory. The changed whitebeast HAProxy origin remains
explicitly pending recapture. Other
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
| IMP-04 | HAProxy ordered `-f` roots and directory expansion | partial | Frontend implemented; full audited lowering remains. |
| IMP-05 | HAProxy defaults/frontend/backend/listen resolution | partial | HTTP/TCP lowering, deterministic environment preprocessing, capacity, health, retry, timeout, and reuse policies exist; audited host gates remain. |
| IMP-06 | Stable blocking diagnostics and provenance in canonical candidates | partial | Canonical provenance plus typed deployment, activation, secret-overlay, environment-fingerprint, and inactive-source records exist. |

## `hostrouter.lan` nginx cases

| ID | Effective behavior | Status | Notes |
| --- | --- | --- | --- |
| HN-01 | 33 HTTP virtual servers sharing IPv4/IPv6 ports 80 and 443 | partial | Host routing exists; per-vhost policy lowering does not. |
| HN-02 | Multiple exact hosts and default-server fallback | partial | Runtime and strict fragment lowering exist; the complete audited candidate remains blocked. |
| HN-03 | Leading-dot and leading-wildcard nginx names | partial | Canonical selectors and runtime longest-suffix precedence preserve nginx semantics; native lowering remains. |
| HN-04 | Nine file-backed certificate lineages selected by SNI on port 443 | partial | Canonical/runtime SNI selection and strict TLS lowering exist; the audited multi-lineage candidate remains blocked. |
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
| HN-16 | Public static document root and index handling | partial | Descriptor-pinned root/alias mapping, ordered try-files, indexes, autoindex, MIME, headers, errors, and ranges run on wire; nginx location reselection and native lowering remain blocked. |
| HN-17 | Header-based bearer authorization | partial | Secret-file bearer and htpasswd policies are integrated; redacted inline nginx authorization lowers only with one uniquely matched secret-file overlay. |
| HN-18 | Cookie path/attribute rewriting | partial | Cookie Path, Secure, HttpOnly, and SameSite rewrites run on wire; native attribute lowering remains. |
| HN-19 | Per-vhost body limits and separate connect/read/write timeouts | partial | Route-local limits and independent deadlines lower and run on wire, but the authenticated complete root remains non-finalizable while unrelated security overlays and native policy are unsatisfied. |
| HN-20 | Mixed buffered and unbuffered proxy paths | partial | Explicit buffering-off uses Pingora streaming; buffering-on fails startup rather than being ignored, and native lowering remains. |
| HN-21 | Gzip type/level policy | partial | Pingora streaming gzip enforces the bounded level and exact content-type policy; native lowering remains. |
| HN-22 | Custom access/error log formats | partial | HTTP file logging emits fixed redacted JSONL; arbitrary source formats, separate error logs, and native lowering remain unsupported. |
| HN-23 | Declared but unapplied nginx rate/connection zones | partial | Parser decisions exist, but the complete native-lowering gate is not met. |

## `hostrouter.lan` HAProxy cases

| ID | Effective behavior | Status | Notes |
| --- | --- | --- | --- |
| HH-01 | Unix frontend socket with explicit mode | partial | Canonical/runtime and strict static-TCP lowering exist on Unix; the complete audited candidate remains blocked. |
| HH-02 | Exact Host ACL and ordered `use_backend` | partial | Exact routes exist; HAProxy first-match lowering does not. |
| HH-03 | `leastconn` across ten backend servers | partial | Runtime physical-connection work/capacity accounting, deterministic ties, and HTTP native lowering are implemented. The live-origin hashed hostrouter evidence consumes a backend-scoped nginx HTTP/1.0/no-keepalive lifecycle overlay; unrelated retry policy still blocks final activation. |
| HH-04 | DNS-named server identities | partial | Canonical/runtime and strict static-TCP lowering exist; the authoritative audited gates remain incomplete. |
| HH-05 | HTTP GET health check, exact 200, interval/rise/fall | partial | Canonical/runtime healthy startup, regular/fast/down intervals, status, version, optional Host, rise/fall, ordered `default-server` inheritance, and native lowering are implemented. Broader health forms remain blocked. |
| HH-06 | `retries 3` and `redispatch` | partial | HAProxy retry lowering targets the same named server with an explicit delay field; enabled redispatch fails closed because its persistence semantics are not equivalent. |
| HH-07 | Client/connect/server/request/keepalive timeout classes | partial | Listener, route-local, and pool timeout scopes are independently enforced and lowered. |
| HH-08 | `forwardfor except` | partial | Canonical/runtime source-CIDR exceptions and the audited loopback form lower; the complete audited candidate remains blocked by unrelated policy. |
| HH-09 | Frontend and process-wide `maxconn` | partial | Aggregate, listener, and server caps are enforced and lower with their distinct scopes. |
| HH-10 | HAProxy stats page and conditional administration | missing | OxiRoute management is a different contract. Stats auth, URI, page, refresh, admin, and exact Prometheus forms remain typed non-equivalent activation requirements. A uniquely consumed operator migration overlay may explicitly select OxiRoute metric families and its broader stats routes; import never widens `/metrics` silently. |
| HH-11 | Syslog and HAProxy HTTP log policy | missing | Fixed redacted HTTP JSONL exists, but syslog and HAProxy-format semantics are absent. |
| HH-12 | user/chroot/daemon process settings | external | Deployment unit or container owns these settings. |

## `phoenix.lan` nginx cases

| ID | Effective behavior | Status | Notes |
| --- | --- | --- | --- |
| PN-01 | Port-80 static root, index files, and custom 50x page | partial | Descriptor-pinned static indexes and custom error documents run on wire; nginx location reselection and native lowering remain blocked. |
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

## Operational observations

- Every currently captured native configuration passed its product's read-only validator.
- `hostrouter.lan` nginx reports deprecated/redefined HTTP/2 listener options and a type-hash sizing
  warning.
- `phoenix.lan` nginx reports a type-hash sizing warning.
- No process was listening on `phoenix.lan` TCP port 1936 during the audit although RTMP push targets
  it.
- One nginx virtual host contains an inline bearer credential. The value is not retained here and
  must be represented by a secret reference before native lowering can be supported.
