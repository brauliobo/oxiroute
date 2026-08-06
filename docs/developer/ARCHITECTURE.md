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
format inference, bounded generic decoding, template expansion, native-reference extraction, and
deterministic rendering. `oxiroute-import` owns product-specific syntax, provenance, decision ledgers,
diagnostics, and conservative lowering.

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

The cache crate, forward-proxy protocol foundations, Varnish IR, and managed ACME components are
useful code, but a public feature claim must follow their integration into the active generation path.
