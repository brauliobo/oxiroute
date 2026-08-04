# Parser Fuzzing

This directory is an isolated cargo-fuzz workspace. Its path dependencies point at existing OxiRoute
crates, but it is not a member of the application workspace and adds no runtime dependency or
application behavior.

## Tools

- Rust `1.87` and Cargo `1.87` are the MSRV compile gate.
- `cargo-fuzz` is required to execute libFuzzer targets. Its normal execution path requires a
  nightly Rust toolchain in addition to the MSRV toolchain.
- A host C/C++ compiler and LLVM tooling are required by `libfuzzer-sys` on the target platform.
- The harnesses do not open sockets, resolve names, read configuration files, or access the network.

The stable compile/list checks do not claim fuzz coverage. The optional CI smoke exits successfully
when `cargo-fuzz` or nightly Rust is unavailable.

## Targets

| Target | Public parser surface | Input cap |
| --- | --- | ---: |
| `config_source` | Restricted Lua, KDL, HOCON, UCI, and template expansion | 128 KiB |
| `native_source` | In-memory nginx, HAProxy, Apache, Squid, and Varnish syntax | 128 KiB |
| `forward_target` | Absolute-form and classic CONNECT authority parsing | 16 KiB |
| `overread_io` | `OverreadIo` prefix-before-underlying-stream behavior | 16 KiB |
| `rtmp_handshake` | Incremental public `rml_rtmp` handshake parser | 128 KiB |
| `rtmp_chunk` | Bounded `rml_rtmp` chunk and message decoding, including AMF message forms | 256 KiB |
| `rtmp_amf` | Direct AMF0 and RTMP AMF message decoding | 32 KiB |

Each parser receives a fresh state per input. The RTMP target limits are intentionally below the
production protocol ceilings so a local smoke run remains bounded; the production parser limits
remain the owning behavior.

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

- UDP datagram parsing is unsupported because OxiRoute has no public UDP datagram parser; UDP is
  currently a configuration/listener validation boundary rather than an active datagram path.
- TLS ClientHello parsing is unsupported because the runtime delegates it to private Pingora/OpenSSL
  integration and exposes no standalone public parser API.
- PROXY protocol parsing is unsupported because no public OxiRoute PROXY protocol parser exists.
- Full HTTP/1 wire parsing remains outside these targets because the runtime parser is owned by
  Pingora/Hyper. The public target parser and over-read adapter are fuzzed directly without network
  I/O.

These gaps are recorded boundaries, not coverage claims.
