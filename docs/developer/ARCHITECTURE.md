# Architecture

OxiRoute separates source interpretation, typed planning, runtime generations, and operational
surfaces. A source is never allowed to mutate a live listener directly.

## Runtime Shape

```text
KDL / Lua / HOCON / UCI / native references
                  |
       bounded source resolver
                  |
      typed canonical configuration
                  |
       validation and runtime planning
                  |
       prepared immutable generation
          /          |          \
    HTTP/TCP       RTMP       control APIs
      data plane   media      monitoring/topology/config
          \          |          /
              Vue/Pug UI and CLI
```

## Source And Configuration

`oxiroute-config` owns the strict canonical model and validation. `oxiroute-config-source` owns
format inference, restricted Lua loading, bounded generic decoding, template expansion,
native-reference extraction, and deterministic rendering. `oxiroute-import` owns product-specific
syntax, provenance, decision ledgers, diagnostics, and conservative lowering.

The resolver pipeline is:

1. Read a stable, bounded root snapshot without following a root symlink.
2. Infer the adapter from the path extension.
3. Decode a generic value tree where applicable.
4. Expand templates and resolve explicitly named native sources.
5. Compose only complete, finalized candidates.
6. Deserialize the canonical model, apply defaults, normalize, validate, and render.
7. Hash the effective deterministic KDL for the candidate/runtime revision.

The exact authored bytes remain the disk revision for optimistic concurrency. These two revisions can
diverge when a native dependency changes without the root file changing.

## Generation Lifecycle

`oxiroute-server` prepares a complete candidate: routes, pools, TLS identities, static roots, secrets,
recording stores, health supervisors, management assets, statistics credentials, and listener
reservations. It publishes only after the candidate is ready. The active and previous generations
remain observable, and admitted transports retain a reference until teardown.

```text
disk change or API draft
        -> load
        -> validate
        -> prepare
        -> candidate ready
        -> publish accept gate
        -> drain previous generation
        -> retain status/metrics
```

Failed preparation, bind conflicts, invalid source changes, and failed activation preserve the active
generation. The parent-directory watcher debounces events and periodically reconciles effective
revisions. The cross-process supervision crates provide a staged master/worker replacement protocol
with authenticated typed TCP, Unix, UDP, and QUIC/H3 listener descriptor adoption; unsupported
listener topologies remain on the direct runtime rather than using an untyped fallback.

## Data Plane

The server crate owns listener admission and dispatches by protocol:

- HTTP reverse proxy routes host/path/method requests into immutable service and pool plans.
- Plain-HTTP HTTP/1.1 listeners are served by one of three planes, chosen per listener when the generation is prepared
  (`service_plan::DataPlane`, root option `http_data_plane`). The tokio plane is Pingora's listener, the fast lane and the
  classic path. The epoll and io_uring planes (one engine, `oxiroute-epoll-h1`, adapted by `http/proxy/engine`, on two
  drivers) are a thread-per-core event loop
  that serves the routes the plan marks `engine` and hands every other connection, with the bytes already read, to the
  tokio plane before any origin is contacted. The adapter reuses the lane's route selection, request context, leases,
  head rendering and access log; it joins the generation's accept gate (pause on publication, drain on retirement) and
  the listener's admission, and its connections hold the same admission guards as tokio ones. `DataPlanePolicy` is
  the one place that decides which plane serves a listener, and it decides once per generation. The chain is data:
  `DataPlane::DEFAULT_ORDER` is the ordered list (`epoll`, `tokio`) and each plane is a `Link` in
  `service_plan/data_plane/chain.rs` with the host capability it needs and what it serves (fast lane, scheduling, TLS only
  when the plane is chosen and the profile is rustls, plain HTTP/1.1 socket listeners). Per listener the first plane whose capability and conditions pass wins, and the skipped
  planes with their reasons travel with the choice (`PlaneSelection`) to the log line written when the listener is
  built. An explicit `epoll` or `io_uring` is a plan error when its capability or conditions fail, an explicit `tokio`
  needs nothing, and an explicit plane left out of the default order still works. The generation blueprint compiler
  stays pure: it receives the host's `PlaneCapabilities` as an input and never probes the kernel. The runtime plan
  detects them once per process at the host boundary (`service_plan/data_plane/host.rs`), only for the planes the
  setting can reach (an omitted setting detects `epoll` and never creates an `io_uring` ring; the `io_uring` check
  runs once on a short-lived ring when the setting names it), and tests feed synthetic capabilities. To put `io_uring` first, add `Self::IoUring` at the head of
  `DataPlane::DEFAULT_ORDER` and the `io_uring` step to the chain sentence in `docs/CONFIG_SPEC.md` and
  `docs/OPERATIONS.md`. An engine thread that panics or fails marks the generation's runtime failed instead of
  leaving a listener that accepts connections nothing serves. Connection placement, `aged_per_worker` and `dynamic`
  worker scaling run inside the epoll engine (`plane/schedule`): the engine places every accepted connection on the
  thread with the fewest open ones (the listener is shared, and the kernel hands a burst of connections to the thread
  it woke, so accept order is not a placement), the ready order is `oxiroute-aged-order`, shared with the tokio
  executor, the active-set decision is `oxiroute-worker-scaling`, and a connection moves between threads as an owned
  descriptor handed through the target thread's inbox while it holds no request, no buffer and no origin
  connection; the engine refuses to start the `io_uring` driver with scheduling.
  The engine's I/O goes through a driver: readiness (`epoll`) or completions of a per-thread ring owned by
  `oxiroute-uring` (`io_uring`). The state machine, `Service` trait, timer wheel and tests are shared; the io_uring
  driver turns completions into the readiness the state machine consumes and owns the buffers of operations in
  flight. A response the ring accepted is sent whole or the connection fails; a closed connection
  waits up to `Config::flush` (a minute) for the peer to take it and is then reset, never ended with a clean end of
  stream. A listener that runs out of descriptors waits for a waiting connection and sheds it through a spare
  descriptor instead of retrying, so it neither spins nor stops. A response body of known length or in chunks is not
  copied in user space: the engine frames the bytes in the receive buffer the ring lent it, and the send of the client
  reads that same buffer, which returns to its pool when the send ends. A socket whose receives keep filling the small
  buffers receives into larger ones (`Config::large_buffers`), so a 64 KiB body takes a few completions, not many.
  A wait returns every completion that is ready, so connections that became ready together are served in one batch
  and one order. Measurements are in `benchmarks/reports/io-uring-http1/README.md`.
- Forward proxy parsing and policy are supplied by `oxiroute-forward-proxy`; the runtime authenticates,
  resolves, authorizes, and connects only approved destinations. Socket-bound H1 listeners terminate
  TLS before this policy path, require negotiated `http/1.1`, and bound the transport handshake by
  the service's idle/lifetime minimum while retaining listener and generation admission.
- TCP relay preserves opaque bytes with independent half-close, backpressure, and bounded deadlines.
- TLS plans select exact/wildcard/default identities and explicit downstream/upstream protocol policy.
- Health supervisors publish observed endpoint state to both selection and monitoring.

Retries, capacity permits, DNS address order, health eligibility, and route precedence must be shared
between the runtime and the observable snapshot. A value that affects selection should have an
observable representation or an explicit reason why it is not exposed.

## Unsafe Containment

Every first-party crate forbids `unsafe` except `oxiroute-allocator`, `oxiroute-cpu` and `oxiroute-uring`. The io_uring
layer cannot be written without it (submission queue entries hand the kernel raw pointers, provided buffer rings are
shared memory), and no safe crate provides the operations it needs, so `oxiroute-uring` is the one crate that opts
in and the engine stays unsafe-free. All unsafe code lives in `crates/oxiroute-uring/src/raw.rs`, behind a safe API
that owns every buffer for as long as the kernel may use it; each unsafe block carries a `SAFETY` comment, and
`unsafe_op_in_unsafe_fn`, `undocumented_unsafe_blocks` and `multiple_unsafe_ops_per_block` are denied.
`scripts/verify/unsafe_containment.sh` runs in CI and fails when another crate stops forbidding unsafe, when unsafe
appears outside `raw.rs` in `oxiroute-uring`, when an unsafe block or impl of any of the three crates lacks its
`SAFETY` comment, or when another crate depends on `io-uring` or
references io_uring system calls.

## RTMP Plane

`oxiroute-rtmp` adapts the pinned RTMP transport into a bounded session/catalog model:

```text
handshake -> AMF command session -> publisher/subscriber roles
          -> stream catalog -> bounded fanout
          -> recorder/relay workers -> redacted snapshots
```

The registry owns publisher incarnation and exact stream/recorder identities. Fanout queues are
bounded per subscriber. Recorder workers use descriptor-safe stores, bounded queues, explicit failure
codes, and asynchronous finalization so disk work does not block media dispatch.

## Control Plane

Management handlers serialize typed JSON responses from the active generation, monitoring snapshot,
topology builder, and configuration coordinator. Configuration writes use a disk revision precondition
and complete preflight before durable replacement. The Vue client parses response shapes defensively,
keeps stale data visible after refresh errors, and never renders secret-bearing backend values.
Operational events use a bounded non-durable ring for polling and SSE; control operations also append
bounded redacted records to the separate durable audit store.

## Supervision And Platform Boundaries

`oxiroute-supervision` contains deterministic value types and replacement state machines with no I/O.
The Unix crate validates descriptors and transports credentials. The master owns listener identity,
worker replacement, and the bounded generation-qualified event ring; workers send authenticated
status observations for lifecycle, listener, metric, reload, degradation, and drain state. The
process crate performs authenticated worker spawning and launcher work. Linux-specific process and
`/proc` behavior stays behind platform modules.

On Linux, `oxiroute serve` selects the supervised master only when the fixed packaged launcher is
present and the listener descriptor topology is eligible. The Arch package installs the launcher, so
eligible packaged services are supervised by default; unsupported topologies and development installs
without the launcher fall back to the direct generation runtime. Non-Linux serving is direct.

The shipped replacement wire uses control protocol version 2 and descriptor manifest version 1. The
planned bidirectional lifecycle-control boundary is one coordinated version-3 change: version-3
masters and workers reject version 2 exactly, with no negotiation or dual-protocol compatibility
layer. Manifest version 1 remains independent and is preserved unless descriptor metadata changes.

The cache crate is integrated into reverse HTTP and eligible HTTP/1/H3 forward requests. The shared
cache transaction owns lookup, collapsed-fill leadership, revalidation, stale eligibility, admission,
completion, and cancellation; protocol adapters retain body I/O and reject trailer-bearing responses
from admission. Forward-proxy code has narrow active H2/H3 classic-CONNECT paths and bounded H3
absolute-form forwarding, while Varnish IR and managed ACME each expose bounded partial contracts. A
public feature claim must still follow integration into the active generation path and its failure
evidence.
