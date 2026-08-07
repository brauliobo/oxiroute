# Parser Fuzzing

This directory is an isolated cargo-fuzz workspace. Its path dependencies point at existing OxiRoute
crates, but it is not a member of the application workspace and adds no runtime dependency or
application behavior.

## Tools

- Rust `1.97.1` and Cargo `1.97.1` are the current checked-in MSRV compile gate.
- `cargo-fuzz` is required to execute libFuzzer targets. Its normal execution path requires a
  nightly Rust toolchain in addition to the MSRV toolchain.
- A host C/C++ compiler and LLVM tooling are required by `libfuzzer-sys` on the target platform.
- The harnesses do not open sockets, resolve names, read configuration files, or access the network.

The stable compile/list checks do not claim fuzz coverage. The separate optional `fuzz-smoke`
workflow exits successfully when `cargo-fuzz` or nightly Rust is unavailable.

## Targets

| Target | Public parser surface | Input cap |
| --- | --- | ---: |
| `config_source` | Restricted Lua, KDL, HOCON, UCI, and template expansion | 128 KiB |
| `native_source` | In-memory nginx, HAProxy, Apache, Squid, and Varnish syntax | 128 KiB |
| `forward_target` | Absolute-form, classic CONNECT authority, and RFC 9298 CONNECT-UDP target parsing | 16 KiB |
| `overread_io` | `OverreadIo` prefix-before-underlying-stream behavior | 16 KiB |
| `rtmp_handshake` | Incremental public `rml_rtmp` handshake parser | 128 KiB |
| `rtmp_chunk` | Bounded and incrementally fragmented `rml_rtmp` chunk/message decoding, including AMF forms and an interleaved-fragment seed | 256 KiB |
| `rtmp_amf` | Direct AMF0 and RTMP AMF message decoding | 32 KiB |
| `proxy_protocol` | Public PROXY v1/v2 stream parsing, encoding, and incremental acceptance | 128 KiB |
| `udp_datagram` | Public PROXY v2 datagram-header parsing on bounded datagram inputs | 131,059 B |
| `tls_client_hello` | Public rustls `ServerConnection` ClientHello parsing and resolver normalization | 64 KiB |
| `http1` | Public Pingora HTTP/1 request/response parsing, normalization, and body framing | 128 KiB |

Each parser receives a fresh state per input. The RTMP target limits are intentionally below the
production protocol ceilings so a local smoke run remains bounded; the production parser limits
remain the owning behavior. Transport targets use only deterministic in-memory IO. PROXY stream
acceptance and HTTP/1 sessions use bounded fragmented `tokio-test` streams; they never create a
socket or connect to a peer.

The RTMP chunk harness feeds each bounded input in deterministic fragments, drains at most eight
messages, and applies a valid `SetChunkSize` message only through the public deserializer API. The
checked-in `rtmp_chunk/interleaved-fragments` seed exercises two simultaneously fragmented chunk
streams. `fuzz/smoke.sh` uses a fixed seed and 32 executions per target; this is reproducible local
smoke evidence, not a coverage result or a long-running campaign.

## Commands

From the repository root:

```sh
cargo fmt --manifest-path fuzz/Cargo.toml --check
cargo check --manifest-path fuzz/Cargo.toml --locked --jobs 4
cargo fuzz list
bash fuzz/smoke.sh
```

Run one target for a bounded local smoke:

```sh
cargo fuzz run forward_target -- -runs=128 -max_len=16384 -timeout=2 -rss_limit_mb=256 -malloc_limit_mb=128
```

The checked-in corpus directories are target-specific and contain small malformed inputs plus
representative syntax. Corpus entries beginning with `hex:` are decoded by the harness; this keeps
binary protocol seeds reviewable as text.

## Deliberate Gaps

- `udp_datagram` covers the public PROXY v2 datagram-header parser on bounded datagram inputs only.
  The UDP payload-limit enforcement, session table, admission policy, and private
  `parse_initial_datagram` helper remain deliberate gaps rather than being exposed solely for
  fuzzing.
- `tls_client_hello` covers rustls's public ClientHello wire parser and the public SNI/ALPN resolver
  view used by the HTTP/3 path. The TCP listener's private Pingora/OpenSSL ClientHello integration
  is not claimed.
- `proxy_protocol` covers the public v1/v2 parser, encoder, and incremental stream acceptor. It
  does not claim daemon listener lifecycle, socket behavior, or source-address spoofing behavior.
- `http1` covers Pingora's public HTTP/1 request and response sessions, header normalization, and
  body/chunk framing over memory streams. It does not claim daemon application routing, connection
  lifecycle, or socket-level wire interoperability.

These boundaries are recorded limits, not coverage claims.

The RTMP harnesses do not establish FFmpeg/OBS interoperability, process-level listener behavior,
long-running crash-corpus results, or production evidence. Those remain external or release-gate
requirements documented in `docs/RTMP_SPEC.md` and `docs/ROADMAP.md`.
