# Shared TLS Client Context And Reload Cost

Upstream health checks, canaries, readmission probes and the HTTP data path all open TLS-capable
connections through the vendored pingora connectors. Building a connector used to build an OpenSSL
client context, and every context parsed the whole system CA bundle twice (`SslConnector::builder`
loads the default trust paths, and the connector loaded them again). A health-checked pool paid about
four parses at each startup, reload and config validation; each parse is about 120 M instructions and
about 1.2 MB of retained heap.

## Design

- `vendor/pingora-core` `connectors/tls/boringssl_openssl`: connectors that trust only the system
  store (no private CA file, no client certificate, no key log) share one process-wide context built on
  first use. A connector with a private CA, client certificate or key log still builds its own. The
  duplicate default-path load is gone. Worker processes are replaced on every generation, so the
  context cannot go stale across reloads; a changed system bundle is picked up by the next worker.
- `vendor/pingora-load-balancing` `health_check`: `TcpHealthCheck` and `HttpHealthCheck` hold an
  `Arc` connector and default to one process-wide connector. Probes use fresh connections, so no
  connection state is shared between pools. `set_connector` accepts either an owned connector or an
  `Arc`.
- Forward-proxy services no longer load the default trust paths a second time.

`crates/oxiroute-server/tests/tls_client_context.rs` counts bytes read from a synthetic
`SSL_CERT_FILE` bundle (in a child process) while planning 12 health-checked pools and asserts a
single parse.

## Reload attribution

Measured with `benchmarks/r1-idle-startup` and per-process `perf record` (farm, first reload with an
empty audit store, instructions in user space):

| part | before | after |
| --- | --- | --- |
| old worker, prepare on `generation reload` | about 27 G | about 1.4 G |
| new worker, adopt and activate | about 25 G | about 1.5 G |
| master reconcile | about 1.3 G | about 1.3 G |
| audit appends from the new generation's initial health transitions | about 29 G | about 29 G |

The earlier "unattributed" reload cost is the audit store: every append rescans the store, so a
reload costs more the more the store holds (82 G with an empty store, over 200 G with a full one).
What is left outside the audit store is three parses of the configuration (master, old worker, new
worker, about 1.2 G each on farm) plus a compile that costs well under 0.1 G. Splitting the plan
stage from the acquire stage, or shipping the validated config from the master, would save a small
part of the second term only, so neither was done; the first term is owned by the audit store, the
second by the configuration parser.

## Short-lived outbound connections

RTMPS client connects, VOD origin fetches over HTTPS, RTMP callbacks and ACME requests used to build an
`SslConnector` per connection and parse the bundle each time. They now share the connector of the
`oxiroute-tls-client` crate (`system_trust_connector()`), built on first use. The context is immutable;
certificate and hostname verification are still configured per connection by `connect`, so the trust
policy is unchanged. `oxiroute-rtmp` `client::tests::rtmps_connects_share_one_system_ca_parse` and
`oxiroute-tls-client/tests/single_parse.rs` count bytes read from a synthetic `SSL_CERT_FILE` bundle and
assert a single parse. The forward proxy builds its connector once per plan and is unchanged.

The connection-layer numbers are in `CONNECTION_LAYER_PERFORMANCE.md`.
