# OxiRoute 6.5.1

Released 2026-11-14.

OxiRoute 6.5.1 makes `oxiroute import nginx` work on complete, real-world router configurations: certificate identities
from the issued certificates, nginx's socket-wide HTTP/2 rule, IPv4 and IPv6 listeners on one port, unverified https
origins, nginx cookie path rewrites, `^~` locations, bearer gates served from operator token files and access-log
migration. The proxy, TLS and benchmarking behaviour of 6.5.0 is otherwise unchanged. The release remains pre-alpha and
retains conservative compatibility claims.

## Behaviour changes to know before upgrading

These apply to configurations produced by `oxiroute import nginx`; native configurations are not affected unless they use
the new options below.

- **https upstreams of an imported nginx configuration are no longer verified by default.** A `proxy_pass https://...`
  without `proxy_ssl_verify` lowers as nginx behaves: the connection is encrypted but the origin certificate is not
  checked and, without an overlay, no SNI is sent. Every such origin is reported with the warning
  `W_UPSTREAM_TLS_UNVERIFIED`. To verify an origin, harden it per authority with `--upstream-tls` (or the `upstream_tls`
  option of an `nginx_server` reference); the import of an authority that the overlay does not match fails, so a typo
  never leaves a pool unverified silently.
- **The default `proxy_buffering` lowers to streaming.** An omitted or `on` `proxy_buffering` becomes a route with
  `response_buffering` off and the warning `W_PROXY_BUFFERING_STREAMED`. Clients see the same response, but the origin is
  no longer decoupled from a slow client. `proxy_request_buffering` omitted or `on` keeps request bodies in memory up to
  the route body limit; large uploads need `proxy_request_buffering off`.
- **HTTP/2 is socket-wide, as in nginx.** Any `listen ... http2` flag enables `h2` ALPN for every server on that TLS
  socket; otherwise the `http2` directives of the servers must agree. `W_HTTP2_SOCKET_WIDE` names the servers that gain
  HTTP/2 without having enabled it.
- **`listen N` plus `listen [::]:N` imports as one dual-stack `[::]:N` listener** with the warning
  `W_DUAL_STACK_LISTENER` when both sockets serve the same servers. Any other IPv4 and IPv6 split lowers to two listeners
  (see `ipv6_only` below).
- **Imported certificate names come from the certificate.** A Certbot lineage takes its DNS names from the subject
  alternative names of the public leaf certificate, not from `server_name`; a `server_name` the lineage does not cover is
  reported with `W_CERTIFICATE_NAME_UNCOVERED`. Private keys are never read.
- **`[::]` sockets are dual-stack on every host.** A listener bound to the IPv6 wildcard now clears `IPV6_V6ONLY` itself
  and accepts IPv4 and IPv6 clients regardless of the host's `bindv6only` setting. Use `ipv6_only` for an IPv6-only
  socket.

## Importer capabilities

- **Routes.** `location ^~ <prefix>` lowers as the plain prefix when the server has no regex, named or variable
  location. The Certbot pair of `if ($host = ...) { return ...; }` and a server-level `return` lowers in nginx rewrite
  order.
- **Bearer gates from operator token files.** A literal `if ($http_authorization != "Bearer ...") { return 401; }` gate
  lowers to a bearer-token-file access policy served from a token file the operator provisions per server name
  (`--bearer-token-file SERVER_NAME=PATH`, regular file, mode 0400 or 0600, one printable-ASCII line). The importer never
  reads, stores or prints the literal. A failed check also sends `WWW-Authenticate: Bearer`, which nginx does not.
- **Cookie and Host handling.** `proxy_cookie_path` lowers with nginx prefix matching and the replacement text verbatim,
  attributes included. `proxy_set_header Host $host` keeps nginx's primary-server-name fallback.
- **Access logs.** Omitted and formatted `access_log` directives migrate to the redacted structured access log through
  `--default-access-log-file`, with `W_ACCESS_LOG_MIGRATED` listing the format variables that log does not carry.
  Directives that have no runtime effect report `W_DIRECTIVE_NO_EFFECT`.
- **Whole configuration trees.** Router-sized configurations (dozens of included files and servers) import end to end;
  the canonical configuration size limit for sources, rendered output and the saved file is now 4 MiB (was 1 MiB), and
  the node limit of a configuration tree (400,000, was 100,000) matches that bound.
- **New CLI flags and `nginx_server` options.** `--bearer-token-file` and `--upstream-tls` on the command line;
  repeatable `bearer_token_file` and `upstream_tls` options in KDL, HOCON and UCI `nginx_server` reference nodes.

## New configuration options

- **`ipv6_only` listener option.** A socket bind to `[::]` may set `ipv6_only #true` to serve IPv6 clients alone, so it can
  share a port with a separate IPv4 listener. Default is dual-stack and the option is never rendered when false. Changing
  it on a socket that keeps its address needs a process restart.
- **Unverified upstream TLS.** An upstream pool's `tls` accepts `verify = "none"` (default `"full"`): the connection is
  encrypted, the origin certificate is not checked, `server_name` becomes optional and is sent as SNI only. It is
  rejected with `ca_certificate_path` and with exact HTTP/3, isolates connection reuse from verified pools and is reported
  by `config check` as `W_UPSTREAM_TLS_UNVERIFIED`.
- **Cookie path rewrite `mode`.** `response_cookie_path_rewrites` entries accept `mode = "nginx_prefix"` (default
  `"exact"`) for nginx `proxy_cookie_path` prefix semantics. The `nginx_host` fallback accepts any nginx primary server
  name and the empty string, which omits the header.
- The dashboard configuration editor exposes `ipv6_only`, upstream TLS verification and the cookie rewrite mode.

## Upgrade notes

- No migration is needed for native configurations. A configuration produced by an earlier `oxiroute import nginx`
  keeps its saved form; re-importing with 6.5.1 produces the behaviour above, so review the preview warnings first.
- Review every `W_UPSTREAM_TLS_UNVERIFIED` and `W_PROXY_BUFFERING_STREAMED` warning of a fresh import and decide per
  origin whether to harden with `--upstream-tls`.
- Documentation: [Import specification](IMPORT_SPEC.md), [Configuration specification](CONFIG_SPEC.md),
  [Configuration formats](CONFIG_FORMATS.md) and the [migration guide](user/MIGRATION.md).

## Build

- The release assets are built by the same pipeline as 6.5.0: profile-guided and BOLT-optimised generic x86-64 binaries
  with the unwinding and abort flavours, checked for baseline CPU compatibility and for embedded build paths.
