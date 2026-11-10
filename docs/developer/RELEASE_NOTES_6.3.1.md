# OxiRoute 6.3.1 (draft)

OxiRoute 6.3.1 is a security fix for the HTTP reverse proxy: hop-by-hop fields of a client request, and the fields a
`Connection` header nominates, no longer reach the origin. Nothing else changes.

## Security

- **Hop-by-hop request fields were forwarded to HTTP/1 origins** (present in every earlier release; the product face
  of [RUSTSEC-2026-0390](https://rustsec.org/advisories/RUSTSEC-2026-0390.html), which the audit reports against
  `pingora-proxy` by version). A request carrying `Connection: keep-alive, X-Nominated`, `TE`, `Proxy-Authorization`,
  `Keep-Alive` and similar fields reached the origin with all of them intact. A client could therefore nominate a
  header that the proxy adds (for example `Connection: X-Forwarded-For`) so that a connection-aware HTTP/1
  intermediary between the proxy and the origin, which honours the nomination, dropped the proxy's header before it
  reached the origin, and an origin that trusts that header (an address based authorization) was bypassed. Response
  headers from the origin were already cleaned.
- **Fix.** The proxy now removes from every request, before any other processing, `Keep-Alive`, `Proxy-Connection`,
  `Proxy-Authenticate`, `Proxy-Authorization`, `TE`, `Trailer`, every header any `Connection` header nominates, and
  `Upgrade` unless the request is a valid upgrade handshake. The `Connection` header itself is not forwarded
  (`Connection: close` of the client is still honoured on its own connection and never reaches the origin).
  Headers the proxy sets itself (forwarded headers, request id, `Host`, configured header rules) are added after the
  removal, so a client cannot nominate them away.
- **Still forwarded.** `Authorization`, cookies and every other end-to-end header; `Transfer-Encoding` (it is the
  framing of a chunked request); `Connection: Upgrade` and `Upgrade` of a valid HTTP/1.1 upgrade handshake such as a
  WebSocket (the proxy sends `Connection: Upgrade` and the client's `Upgrade`); `TE: trailers` of an HTTP/2 request
  to an HTTP/2 origin (gRPC needs it).
- **Applies to** HTTP/1.1 and HTTP/2 clients, HTTP/1.1 and HTTP/2 origins, and, for the same headers, the HTTP/3
  reverse path. The forward proxy already removed these fields.
- RUSTSEC-2026-0388 (an HTTP/2 `:authority` that differs from `Host`, userinfo in the authority) was already
  refused by the proxy with 400 for HTTP/1 and HTTP/2; two new tests pin it. The audit still lists it until the
  vendored Pingora is rebased.

## Cost

User-space instructions per request (`benchmarks/r2-data-path`, same host, 6.3.0 and the fix built the same way, three
runs each, the spread of a run is 0.2% to 1.5%): HTTP/1.1 proxied 1 KiB over a keep-alive connection (the harness
client sends `Connection: keep-alive`) 42,120 and 42,060 (no change: the origin no longer gets the header, which pays
for the check); HTTP/2 over TLS 1 KiB 68,470 and 68,710 (no change within the noise); a new connection per request
(`Connection: close`) 54,800 and 55,240 (+0.8%); WebSocket echo 12,044 and 12,052. A request without any hop-by-hop
field costs one flag test; the HTTP/1 parser notes the fields while it appends the headers.

## Behaviour changes to know before upgrading

- A request whose `Connection` header names `Content-Length`, `Transfer-Encoding` or `Host`, or contains an option
  that is not a token (`Connection: two words`, a pseudo-header name), is answered 400 and not forwarded. Removing
  such a field would make the origin read the message differently from the proxy. The same names in the
  `Connection` header of an origin response make the response a 502.
- Empty elements in a `Connection` header (`Connection: , close`) are ignored, as RFC 9110 requires. Before, an
  origin response with one was answered 502.
- An origin no longer receives `Connection: keep-alive` or `Keep-Alive` from a client, nor `Proxy-Authorization`
  or `TE`. HTTP/1.1 origins keep their connection alive without them. An origin that needs a client's proxy
  credentials or `TE` must not rely on this proxy to carry them; use a route header rule with a fixed value.
- A client `Upgrade` header without `Connection: Upgrade`, or sent over HTTP/1.0, is no longer forwarded.
- A client-supplied `X-Forwarded-For` (or any header) named by the client's `Connection` header is discarded
  before the proxy appends its own value.

## Upgrade notes

- No configuration change is needed and no setting was added. A 6.3.0 configuration loads unchanged.
- Restart or reload as usual.

## Vendored Pingora

`pingora-core`: the HTTP/1 request facts also note whether a hop-by-hop field is present (`RequestFacts::hop_by_hop`,
`is_hop_by_hop`), and a session can record the facts of a request whose headers the caller changed
(`set_request_facts`). See `vendor/pingora-core/README.oxiroute.md`.
