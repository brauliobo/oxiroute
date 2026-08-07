# Parser Fuzzing

The checked-in bounded harnesses live in [`../../fuzz`](../../fuzz/README.md). They are isolated from
the application workspace and exercise only public parser APIs with no network or filesystem input.

Use Rust `1.97.1` for the checked-in contract and compile gate. `scripts/verify-fuzz.sh` validates every
manifest target, harness source, corpus directory, bounded seed, decoded `hex:` entry, and recognized
deterministic seed marker before compiling the isolated workspace. Executing libFuzzer targets
additionally requires `cargo-fuzz`, a nightly Rust toolchain, and host LLVM/C++ tooling. The separate
optional `.github/workflows/fuzz-smoke.yml` workflow reports unavailable optional tooling and skips
execution, but fails closed when detected cargo-fuzz/nightly tooling cannot list or run targets.

The harness list and deliberate UDP, TLS ClientHello, PROXY protocol, and full HTTP/1 wire gaps are
documented in [`fuzz/README.md`](../../fuzz/README.md). No fuzz target result is a coverage claim.
