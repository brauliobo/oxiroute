# Allocator selection

Linux builds use jemalloc by default. The allocator is selected at build time and stays the same for the process's
whole lifetime. Other operating systems use Rust's system allocator.

```sh
# Default Linux build: jemalloc.
cargo build --release --locked -p oxiroute

# Linux build using std::alloc::System instead.
cargo build --release --locked -p oxiroute --no-default-features
```

The server forwards its `jemalloc` feature explicitly; its allocator dependency has default features disabled.
For a whole-workspace system-allocator build, use `--workspace --no-default-features`. Cargo unifies features, so
adding `jemalloc` anywhere in the selected dependency graph enables it. `heap-profile` explicitly enables jemalloc
and therefore also selects it when combined with `--no-default-features`.

System builds do not link jemalloc, start its background threads, read `_RJEM_MALLOC_CONF`, or run its purge operations.
Jemalloc builds preserve the existing embedded cache policy and the operator's `_RJEM_MALLOC_CONF` override.

Allocation counting wraps whichever allocator was selected:

```sh
cargo build --release --locked -p oxiroute --features alloc-count
cargo build --release --locked -p oxiroute --no-default-features --features alloc-count
```

Heap profiles require jemalloc's diagnostic build; their memory and timing are not production performance results:

```sh
cargo build --release --locked -p oxiroute --features heap-profile
```

Run the allocator feature checks separately so a workspace's feature unification cannot hide an opt-out failure:

```sh
scripts/check-allocator-selection.sh
```

This checks the server dependency graphs and runs real global-allocation tests with and without counting. System
graphs must contain no `tikv-jemallocator` or `tikv-jemalloc-sys`, and System test executables must have no `_rjem_*`
allocator symbols. Keep compiler, release profile, configuration, CPU placement and workload identical when
comparing allocator performance. Resident memory is not allocated live bytes; an idle connection pool and allocator
caches can retain memory after requests finish without leaking it.

Set `SYSTEM_ALLOCATOR_BINARY` to also verify a built System server executable. Keep its symbol table: stripped or
unreadable executables fail this check because their allocator linkage cannot be verified.

For lower memory usage with jemalloc, operators can reduce the small-object cache limits at startup:

```sh
_RJEM_MALLOC_CONF=tcache_nslots_small_min:20,tcache_nslots_small_max:200 ./target/release/oxiroute
```

A controlled measurement reduced anonymous warm memory by about 18 MiB, but an HTTP/2 workload used 3.02% more user
cycles and served 2.42% fewer requests per second. The default retains the larger caches for speed. Measure the same
workload before choosing this override; reducing only the large-object cache limits did not reproduce the memory gain.
