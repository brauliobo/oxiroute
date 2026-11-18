# Operations

Operator procedures for pools, drains, rollbacks and outages are in the
[runbook](user/RUNBOOK.md).

## CLI

`oxiroute serve CONFIG` starts the daemon. A lone positional `CONFIG` remains accepted for
existing invocations. Offline commands are:

```sh
oxiroute serve /etc/oxiroute/oxiroute.kdl
oxiroute config check /etc/oxiroute/oxiroute.kdl
oxiroute import nginx /etc/nginx/nginx.conf --root-prefix / --emit report
oxiroute import nginx /etc/nginx/nginx.conf --root-prefix / --host-timezone America/Argentina/Buenos_Aires --default-access-log-file /var/lib/oxiroute/http-access.jsonl --recording-root /mnt/cloud/4tb/cam-rtmp --default-error-server nginx/1.30.2 --format kdl --emit preview
oxiroute import haproxy /etc/haproxy/haproxy.cfg --emit preview
oxiroute import haproxy /etc/haproxy/haproxy.cfg --node-ip 192.0.2.37 --gpu1-defined --emit preview
oxiroute import haproxy /etc/haproxy/haproxy.cfg --node-ip 192.0.2.37 --gpu1-defined --shadow-port-offset 10000 --emit preview
oxiroute config compose edge.kdl legacy.lua openwrt.uci site.conf
oxiroute config compose --format hocon edge.kdl legacy.lua
oxiroute version
```

Import `report` output includes diagnostics and unresolved deployment/activation requirements.
`preview` succeeds only for a finalized canonical candidate and writes deterministic KDL by default;
`--format kdl|lua|uci|hocon` selects another rendering.
HAProxy files using `${NODE_IP}` or `defined(GPU1)` require the corresponding explicit
`--node-ip` and `--gpu1-defined` preprocessing inputs; the importer fingerprints those values.
HAProxy `log`/`httplog` and user, group, chroot, daemon, PID, process, and thread directives are
reported as deployment warnings. A finalized preview does not mean those settings were implemented;
the target service unit/container and logging pipeline MUST reproduce the required ownership,
isolation, process topology, and log handling before cutover.
`--shadow-port-offset` is preview-only and shifts imported IP socket listener ports after exact
lowering, then revalidates the complete canonical configuration for side-by-side canaries.
`--recording-root` replaces exactly one native nginx recording root with an explicit canonical
no-symlink path and fails closed when the source has zero or multiple recording roots.
`config compose` loads finalized inputs of any supported syntax in order and rejects conflicting
process-wide settings, duplicate names, and invalid cross-references. It emits deterministic KDL by
default; `--format kdl|lua|uci|hocon` selects another output adapter. This output is a flattened typed
configuration: templates and native references are resolved rather than copied. Run `config check`
on the composed file in the target host environment to verify runtime preparation and referenced
files before activation.

The standalone `import nginx|haproxy --emit preview` commands accept only fully finalized native
candidates and emit deterministic KDL by default. `--format kdl|lua|uci|hocon` selects preview
syntax. The flag does not change `--emit report`; report output remains importer evidence.

`serve` defaults to `oxiroute.kdl`. Extensionless paths and `.kdl`/`.kdl2` use KDL 2.0; `.lua` uses
restricted Lua, `.uci` uses OpenWrt UCI, and `.hocon`/`.conf` use HOCON.

## Generations

Candidate preparation validates and compiles routes, pools, TLS identities, static roots, access
secrets, recording stores, health supervisors, relay policies, management assets, stats credentials,
and all listener reservations before publication. It does not write the canonical file. Matching
listener reservations are process-owned and reused by later generations.

Activation swaps active and previous generation references under one lock, closes the old logical
accept gate, and retains the previous generation for explicit rollback. Before the new generation
accepts, the runtime administrative state of the old one (drain, maintenance, health override,
check switch, max-connections and weight overrides) is copied to the servers with the same pool and server
name, so a drained server never takes traffic across a file write, reload, apply or rollback.
`evacuate` sets the same `drain` state; its one-shot cancel of in-flight requests is never carried over.
Servers that no longer exist lose their state and new servers start ready. In supervised mode the
old worker reports that state at quiesce and the master hands it to the replacement at activation;
a state too large for the control channel (over 48 KiB, hundreds of non-default servers) refuses
the replacement and the old worker keeps serving. HTTP/1, HTTP/2, WebSocket,
TCP, UDP pseudo-sessions, QUIC/H3 connections, and RTMP references are counted independently and
drained under a caller-supplied deadline. Failed candidates are dropped without changing active
state. Canonical management writes remain revision-checked and invalid drafts do not alter disk.

The watcher observes the parent directory so rename-based replacement is visible, but only an event
that names the configuration file, the directory itself or a recorded native dependency starts a
reconciliation (scratch files, editor swap files and tooling output next to the configuration are
ignored). It debounces event bursts and periodically reconciles exact SHA-256 disk revisions. Invalid snapshots are rejected and
do not replace active generation state: each one is logged at error level on the `oxiroute::config`
target (once per distinct rejection, not at every reconciliation), emits a `configuration_reload`
event with the outcome `rejected`, raises `oxiroute_config_rejections_total` and is shown as
`lastRejection` in `generation status` and `GET /api/v1/generations` until a later file is accepted,
in the plain and the supervised runtime. The master reports a rejection to the active worker, so
`lastRejection` also covers a reload the master refused (unsupported configuration, a listener that
cannot be bound) or whose replacement worker failed. `degraded` is true while a rejection is
unresolved; `GET /ready` stays 200 because the active generation keeps serving. `config check`, `422` responses
and startup failures name the failing setting (`path`, `detail`, `line`). Operational logs on
the `oxiroute::operations` target are
structured JSON and never include source text, secret values, configured paths, or OS error strings.

Reconciliation reads and hashes the file every time but resolves it only when its bytes changed: the
decoded document is reused while the SHA-256 revision is unchanged, so an unchanged 900 KB
configuration costs about one million instructions per 30-second reconciliation instead of about
three billion. For compositional KDL/HOCON/UCI roots with native references, periodic
reconciliation re-resolves the native references even when the root bytes are unchanged. A native change can therefore produce a new
candidate revision while `diskRevision` remains unchanged. Native import failure diagnostics crossing
the management boundary contain stable code counts, not source text, paths, or native diagnostic
messages.

A generation reads the files the configuration points to when it is prepared, so their content is part
of the candidate revision: a digest of the management token file (`management.token_file` or
`OXIROUTE_MANAGEMENT_TOKEN_FILE`), `stats.admin_token_file`, `registry.tokens_file`, the bearer token and
htpasswd files of routes, forward proxies and cache purge, the client CA (`client_auth.ca_certificate_path`),
`dh_parameters_path`, the upstream `ca_certificate_path`, RTMP secret files and the ACME DNS credential
file is folded into it. A rewritten file therefore makes the next reconciliation, `SIGHUP` or
`generation reload` replace the generation without any edit of the configuration, and `diskRevision`
stays unchanged. A missing file digests differently from any content, so a file that is removed
and written again passes through a refused candidate. Certificate chains and private keys are not
part of it: they are reloaded in place. A configuration that points to none of these files has the
revision of its normalized form alone. In supervised mode a `generation reload` request makes the
master reconcile at once.

Configuration sources are privileged administrator input. HOCON includes and process-environment
fallbacks are disabled, UCI is parsed as data without shell execution, and Lua has no standard
libraries. Native references are the deliberate exception to no-I/O parsing: they read the named
nginx/HAProxy roots and their importer-defined include graphs with the daemon account's filesystem
permissions. They do not execute native binaries, shell expansions, or infer process environment.

The supervised launcher transfers authenticated typed listener descriptors across the master/worker
boundary for TCP, Unix, UDP, and QUIC/H3 listeners. It does not claim arbitrary inherited-file-
descriptor upgrade; unsupported descriptor topologies remain on the direct runtime, and listener
reuse during supervised replacement requires an unchanged typed manifest. During replacement, the
old generation stops new admissions before the candidate activates. Existing UDP pseudo-sessions
and H3 requests remain owned by the retired worker; H3 sends GOAWAY and rejects later requests,
while UDP keeps the existing session on the retired generation and admits new sessions only after
the candidate commits. A rejected candidate leaves the active worker and its listener ownership
unchanged.

The master supervises one serving worker and does not restart it. When the active worker exits outside a
shutdown or replacement (a signal such as `SIGKILL`, a crash, or a closed control channel), the master
terminates the worker's process group, logs `supervised active worker lost (<phase>)` and `supervised active
worker <id> exited: <status>` at error level, and exits with status 1, so the service manager restarts the
daemon (`Restart=on-failure` in the packaged unit). A crashed candidate during a reload is rolled back to the
active worker and a crashed retired worker completes the reload; neither stops the daemon. This is fail-closed
by design: a daemon whose listeners nothing serves is never left running.

## Idle behaviour

An instance with no traffic does no periodic work and takes no wakeups of its own. The supervising master sleeps
until a worker sends a frame, a worker exits, a signal or a configuration file event arrives, or the next
deadline falls due (the 30-second configuration reconciliation, a pending replacement phase, the
once-per-second report of a draining worker). The worker sleeps on its control channel and on a process
doorbell that whatever can change its status rings: a connection arriving at an idle worker, a listener or
administrative state change, an operational event, a closed accept gate, a failed or finished runtime. It sends
its status when one of them changed it, every 250 ms while counters or events change or a retired generation
drains, and every second only while connections are open or a UDP listener counts datagrams. A request from the
master is served at once. The direct runtime sleeps on the same doorbell for signals, reload requests, candidate
generations and retiring ones. The status carries traffic and listener counters only; process, host, pool and
certificate sampling happens when the management API or `/metrics` asks for it. The ACME renewal thread exists
only when a certificate is managed by ACME, the in-flight request watchdog sleeps until the earliest request
that could go quiet (and while none is registered), and the shared Pingora timer thread sleeps until its
earliest armed timer (and while none is armed).

## TLS handshake threads

TLS handshakes cost 0.6M to 1.7M instructions each (an RSA-2048 certificate costs 15 times the signature of an ECDSA P-256 one),
so they run on `tls_handshake_threads` threads (default one per 8 CPUs, 1 to 4) instead of on the listener workers; a
burst of new connections then adds latency to the connections being served only by the CPU it takes, not by queueing them
behind handshakes. A busy handshake thread shows in `top -H` as `downstream TLS offload`. Raise the value when
a listener takes thousands of new TLS connections per second and those threads are saturated; set `0` to handshake on the
workers. A change is an ordinary reload (the new generation starts its own threads, the replaced one finishes its handshakes
on its own). Handshakes waiting for a thread count against `max_connections` and the handshake timeout.

## HTTP/3 congestion control

HTTP/3 listeners use the BBR congestion controller by default (`http3_congestion_control = "bbr"`). It keeps a download
fast on paths with random loss or a long round trip, where `cubic` collapses: a 10 MiB download over a 100 ms round trip
with 1% loss takes under a second with `bbr` and about a minute with `cubic`. It is not neutral: on a bottleneck shared
with `cubic` HTTP/3 or TCP flows the BBR flow takes nearly the whole link, it drops many of its own packets at shallow
buffers, uses more CPU per MiB, and a loss-free LAN bulk transfer is several times slower than with `cubic`.

Set `http3_congestion_control = "cubic"` when the listener serves LAN or data-center clients, when its large transfers
share a bottleneck with other traffic you must not starve, or when you watch CPU per MiB or packet loss on the link. A
change is an ordinary reload: new connections of the new generation use the new controller, established ones keep theirs.
The measured numbers and how to repeat them are in the [setting description](CONFIG_SPEC.md) and in
`benchmarks/r2-data-path/README.md`.

## HTTP/2 receive windows

An `http` or `forward_http2` listener advertises `h2_initial_stream_window` (default 1 MiB, 65535 to 16 MiB) as each
request stream's window and `h2_initial_connection_window` (default 8 MiB, 65535 to 64 MiB, not below the stream window)
as the connection's. A client may send that much before the proxy has read it, and the proxy gives credit back only as
it reads, so the windows are the memory bound: a connection whose requests are stalled (an origin that is slow to accept
the body) holds at most its connection window of request data, whatever the number of streams, and a stream at most its
stream window. At the defaults that is 8 MiB per connection, against 64 KiB before 6.4; budget
`max_connections * h2_initial_connection_window` for the worst case and lower the options on a listener that faces many
untrusted connections (idle and ordinary connections hold nothing extra: nothing is allocated up front). The larger
windows cut the `WINDOW_UPDATE` frames of an upload from 64 per MiB to about 2 (each was a TLS record and a write) and
raise the per-stream ceiling from 64 KiB per round trip to 1 MiB per round trip (168 Mbit/s at 50 ms). Raise the
stream window for high-latency clients that upload large bodies; downloads depend on the client's windows, not these.
A change is an ordinary reload; connections already open keep the windows they were opened with.

## TLS handshake cost

A full TLS 1.3 handshake costs about 0.66 million user instructions on the default rustls backend and about 1.7 million
on the OpenSSL backend with an ECDSA P-256 certificate (about 200 us of CPU on a current core with OpenSSL), and 4.6 million
with an RSA-2048 certificate on OpenSSL, because the RSA signature is 3.1 million against 0.2
million. Prefer ECDSA certificates on listeners that take many new connections (`key_type = "ecdsa_p256"` is the default
of the managed and self-signed sources; a Certbot or file certificate takes the key it was issued with, so request an
ECDSA certificate from Certbot with `--key-type ecdsa`). A profile serves one certificate per name, so the choice is
per name: use RSA only where clients that cannot verify ECDSA must be served.

Three profile options (`tls_profiles[].policy`, see `CONFIG_SPEC.md`) change the cost of connections that arrive:

| Option | Effect | Cost |
| --- | --- | --- |
| `key_exchange_groups = { "x25519", "secp256r1" }` | a browser handshake costs 1.58 million instead of 2.27 million instructions (-30%): browsers offer the post-quantum hybrid `x25519mlkem768`, which the default list prefers and which costs 0.68 million more | the key exchange is classical only: recorded traffic is not protected against a future quantum computer; default unchanged |
| `session_cache = { name, size_bytes }` | a TLS 1.2 client that reconnects resumes at 255 thousand instead of 1.5 million (-83%); a TLS 1.3 client at -6% to -14% (the key exchange still runs) | memory of `size_bytes` per profile, shared by the workers of the process; off by default |
| `session_tickets = true` | stateless tickets, no server memory | one ticket is issued per full handshake (about 5% of its instructions) and a fresh one on every resumed connection; a resumed TLS 1.2 handshake costs 79% less than a full one, a resumed TLS 1.3 handshake only 7% less (it still does the key exchange); off by default |

Handshake CPU is the sum over new connections, so the effect shows in `top -H` as the `downstream TLS offload` threads
(one thread handles about 4,800 ECDSA handshakes per second, see `tls_handshake_threads` above). A keep-alive or HTTP/2
connection pays the handshake once: with 50 requests per connection the handshake is under 40% of the connection's CPU,
with 1,000 requests it is below 5%, and the options above matter for short connections only.

## Kernel TLS

`tls_profiles[].policy.ktls` is `"auto"` by default. A TLS 1.3 AES-GCM HTTP/1.1 connection that sends its first
large static file (about 256 KiB and up) is switched to kernel TLS and the file goes out with `sendfile`; every later
response on that connection is encrypted by the kernel as well. It needs the `tls` kernel module and kernel 6.5 or
newer. The module is not loaded by default and an unprivileged service cannot load it, so it must be loaded before
OxiRoute starts, and the configuration reloaded afterwards (the probe runs per generation). The Arch packages
(`oxiroute`, `oxiroute-bin`) ship `/usr/lib/modules-load.d/oxiroute.conf` and load it at boot; mask it with an empty
`/etc/modules-load.d/oxiroute.conf` to keep it unloaded. On other distributions:

```sh
echo tls | sudo tee /etc/modules-load.d/ktls.conf   # at boot
sudo modprobe tls                                   # now
grep -E 'TlsTxSw|TlsCurrTxSw|TlsDecryptError' /proc/net/tls_stat   # converted since boot, alive now
```

Without the module, or with an older kernel or an unsupported cipher, `"auto"` behaves exactly as
`"off"`; the log of the profile says once (info) whether kernel TLS is active or why not. Set `ktls = "off"` to keep
all TLS in user space. A converted connection that then serves small responses costs 1% to 4% more instructions per
request; the break-even is one large file per 200 small requests.

The `rustls` backend converts TLS 1.2 and TLS 1.3 connections (AES-128-GCM, AES-256-GCM, ChaCha20-Poly1305) and the
kernel then encrypts and decrypts both directions. A connection stays in user space when the peer has sent plaintext
that was not read yet, half a record, or the next request ahead of the conversion; a later large file asks again.

The kernel does not change keys by itself, so the server does it for converted connections: a TLS 1.3 `KeyUpdate` from
the client is followed (and answered when it asks for an answer), and the key the server sends with rolls with a
`KeyUpdate` after 2^23 records, half of the AES-GCM confidentiality limit of RFC 8446. TLS 1.3 connections are
converted only when the kernel accepts a second key on a running connection (Linux 6.14 and newer, checked at start);
older kernels still convert TLS 1.2 and keep TLS 1.3 in user space. A TLS 1.2 connection cannot change keys: it ends at
2^24 records with AES-GCM, as it does in user space, and a renegotiation request from the client ends it.

## TLS backend

A TLS profile runs on rustls with aws-lc-rs unless it needs OpenSSL. `tls_profiles[].policy.backend` is normally left
out: the profile then gets `"rustls"`, or `"openssl"` when it sets `dh_parameters_path`, lists a `DHE-RSA-` suite in
`cipher_list`, or uses a `cipher_list` that leaves rustls no TLS 1.2 suite; the log says once, at `info`, which setting
selected OpenSSL. `backend = "openssl"` always selects OpenSSL, and `backend = "rustls"` with one of those settings
stops the start or reload with an error naming the setting. A connection never changes library. rustls costs about 60%
fewer instructions per full handshake and 3% to 14% fewer per keep-alive or HTTP/2 request, and about half the memory of
an idle TLS connection.

rustls offers the TLS 1.3 and ECDHE AEAD suites only and has no FIPS module and no finite-field DHE. A profile that used the OpenSSL default list therefore no longer offers its
DHE-RSA suites once it runs on rustls; clients that connect with
only those fail the handshake.

### Cipher macros

`cipher_list` accepts an OpenSSL cipher string. On the default backend a macro such as `HIGH:!aNULL:!MD5` is evaluated by
OpenSSL, the TLS 1.2 suites it selects that rustls implements are offered in OpenSSL's order, and the rest (DHE, CBC, RSA
key exchange) are not offered. The log names them once at `warn`, with the setting to change: `policy.cipher_list` and
`policy.backend`. A macro alone never selects OpenSSL; an explicit `DHE-RSA-` suite, `dh_parameters_path`, FIPS or forced
kernel TLS do. A macro that leaves rustls no TLS 1.2 suite, such as `LOW`, selects OpenSSL.

An nginx `ssl_ciphers` macro is imported as the profile's default cipher policy instead (`cipher_list` omitted), with the
`W_TLS_CIPHERS_DEFAULTED` note; the mapping table is in the nginx TLS section of `IMPORT_SPEC.md`.

### Serving legacy clients

Give the legacy clients their own listener and profile on OpenSSL, and keep the main profile on rustls:

```lua
tls_profiles = {
  {
    name = "public",
    certificates = { "example" },
    default_certificate = "example",
  },
  {
    name = "legacy",
    certificates = { "example" },
    default_certificate = "example",
    policy = {
      backend = "openssl",
      cipher_list = "ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:DHE-RSA-AES128-GCM-SHA256",
    },
  },
}
```

Bind the `legacy` profile to its own listener address. Operators who want a single profile on OpenSSL, as before, set
`backend = "openssl"` on it; a reload applies a change to new connections, so compare the handshake and error counters
before moving more profiles.

## Shutdown

Pingora is configured with a 3-second connection grace period followed by a 2-second runtime
shutdown deadline. SIGTERM requests graceful shutdown; packaging uses a 15-second systemd stop
deadline. Tests send SIGTERM and reserve force-kill only as a bounded test cleanup fallback.

Automatic ACME cancellation is cooperative. DNS resolution, connect, TLS handshake, socket I/O,
and polling/sleep loops check cancellation within their configured scheduler interval (at most 50
ms for network and poll waits). Local filesystem/state/fsync and OpenSSL calls, and arbitrary
in-process DNS-provider calls, cannot be preempted; they check before/after when control returns and
providers receive the operation context for cooperative checks. The generation process still applies
the existing five-second orchestration deadline and may detach its generation thread. If a local or
provider call remains blocked, ACME authority and resources are released eventually after it returns;
ACME creates no hidden detached helper. Cancellation before confirmed DNS cleanup leaves the durable
pending journal for recovery, while confirmed cleanup removes it exactly once.

## Readiness

`GET /ready` returns 200 only when an active non-degraded generation exists that admits traffic (an operator
`generation drain` makes it return 503 until a reload replaces the generation) and all configured
traffic listeners report `listening`; otherwise it returns 503. `GET /api/v1/status` reports the
build version, disk/candidate/active/previous revisions, active-generation age, listener states,
component states, certificate expiry data, and audit component degradation (`lastError` names the cause);
its `degraded` is true while the generation or any component is degraded. Process/host sampling
is healthy on Linux x86_64 and aarch64; other platforms keep status available with explicit
`unsupported` process/host component states and null unavailable samples.

Every recognized `/api/v1` or `/api/v2` route, including monitoring, topology, RTMP, listener/pool/server,
generation, TLS, process, configuration, audit, and event routes, requires exactly one management Bearer
token. The only public recognized API probes are exact `GET /ready`, `GET /metrics` and `GET /status.json`. Event
operations preserve their shipped v1 polling/SSE contract at `/api/v1/events` and
`/api/v1/events/stream`. The corrected contract is at `/api/v2/events` and
`/api/v2/events/stream`; both versions also negotiate SSE on their page path with
`Accept: text/event-stream`.
There is no unbounded event queue. Durable audit history is separate from the event ring and is
available through authenticated `/api/v1/audit` and `/api/v1/audit/status`.

`GET /metrics` exports process/host sampling state and values, active-generation age, listener,
pool (including fail-open selections), server, queue, health, retry, certificate, RTMP
relay/recording, generation, and bounded audit persistence families in Prometheus text format.
Unsupported process/host samples are omitted rather than exported as zeroes.
`/stats` provides a compact
read-only HAProxy-oriented pool/server view.

`GET /status.json` is a public, secret-free summary for dashboards and the future `oxiroute check`:
`state` (`ok`, `warning`, `critical`), the generation age and whether the last configuration file
was refused, one entry per pool (`state`, `available`, `total`, `ejected`, `failOpen`, `queued`,
`oldestQueueWaitMs`, `inflight`, `oldestInflightMs`), and the number of overdue and stuck requests.
A pool with no available server is critical; a pool with a server missing, or whose oldest queued
request has waited over five seconds, is a warning; an overdue request makes the whole status
critical.

### Hop-by-hop headers

The HTTP reverse proxy removes the hop-by-hop fields of RFC 9110 section 7.6.1 from every client request before it
is routed or any header rule runs, so the origin sees only end-to-end fields and the ones the proxy adds: `Keep-Alive`,
`Proxy-Connection`, `Proxy-Authenticate`, `Proxy-Authorization`, `TE`, `Trailer`, every header named by a
`Connection` header, and the `Connection` header itself. `Connection: close` ends the client's connection and is not
sent to the origin. `Upgrade` and `Connection: Upgrade` pass only for a valid HTTP/1.1 upgrade handshake (WebSocket);
`TE: trailers` of an HTTP/2 request passes to an HTTP/2 origin. The same fields are removed from origin responses
(except a `101` response, which keeps its handshake). A request whose `Connection` header names `Content-Length`,
`Transfer-Encoding` or `Host`, or holds an option that is not a token, is answered 400; such an origin response is a
502. The proxy's own headers, such as `X-Forwarded-For`, are added after the removal, so a client cannot nominate them
away.

### HTTP/1.1 fast lane

Plain HTTP/1.1 requests of fixed-response and redirect routes and of proxy routes are served on a compiled lane that
answers them on the connection itself, with the same bytes, headers, framing and connection handling as the classic
path. Proxy routes may have header rules, retries of `GET` and `HEAD`, queue classes, an HTTP/1.1 TLS origin and a
service access log, and a request body that arrives with its head. Rescue pools, cache, affinity, path and cookie
rewrites, buffering, gzip, streamed or chunked bodies and `Expect` take the classic path from the first byte. The root option `http_fast_lane = false` sends every request through the classic path (for a comparison
or to rule the lane out); it is an ordinary reload, applied to the new connections of the new generation.

### HTTP/1.1 epoll data plane

On Linux, a plain-HTTP (non-TLS) listener serves proxy routes that the fast lane serves from a thread-per-core epoll
event loop instead of the tokio runtime. A route qualifies when every attempt can be started without waiting: its
servers are fixed socket addresses reached over plain HTTP, the pool never queues a request, and retries have no
delay. Selection, retries, health signals, connection limits, the access log, request ids, header rules and metrics
are the lane's own, so a request is accounted identically on either plane. Every other connection (TLS, HTTP/2 and
HTTP/3, upgrades, requests with `Expect` or a chunked body, routes that do not qualify, a pool that has no server
ready) is handed to the tokio path with the bytes already read. The root option `http_data_plane` chooses the plane
(`epoll`, `io_uring` or `tokio`). Omitted picks, per listener, the first available plane of the chain `epoll`,
`tokio` (`io_uring` only when chosen), decided once when the generation is prepared; the server logs at info level, once per listener, the
plane chosen and why each better plane was skipped (`kernel.io_uring_disabled`, a seccomp or container policy, a
missing kernel operation, `http_fast_lane = false`, `aged_per_worker` scheduling for `io_uring`, or a TLS (served by `epoll` only when chosen
and the profile uses rustls), HTTP/3 or Unix-socket listener). An explicit `epoll` or `io_uring` is rejected, naming the missing capability, on a platform,
kernel or sandbox that cannot run it and with `http_fast_lane = false`, and is never replaced silently; `tokio` forces
the tokio path. The engine threads are named `uring-h1-N` or `epoll-h1-N` after their plane. The plane honours
`connection_scheduling = "aged_per_worker"` with `connection_age_ms` and `worker_scaling = "dynamic"` on its own
threads (epoll only): a thread serves its ready connections newest first and the oldest first once it has waited the
age, and with dynamic scaling the threads that leave the active set stop accepting and hand their idle keep-alive
connections to an active thread between two requests. With two threads or more the plane places every new connection
on the thread with the fewest open connections, whatever the scheduling, so a burst of connections does not stay on the
one thread the kernel woke to accept it. The `oxiroute_aged_queue_*` and `oxiroute_worker_scaling_*` metrics are the same on every plane. The option
is an ordinary reload.

A client that sends requests back to back on one connection without waiting for the answers is served in order by the
epoll plane; the classic path closes such a connection after the first answer. When a thread of the plane panics or
fails, the generation is marked failed and the supervisor replaces the worker. When the process runs out of file
descriptors, the plane closes the connections it cannot accept and keeps serving the others, instead of stalling.

The epoll plane relays a response body framed by `Content-Length` of at least 16 KiB from the origin socket to the
client socket with `splice(2)` through a per-thread pool of 64 pipes of 64 KiB, instead of copying it through the
connection buffers; chunked and close-delimited bodies keep the buffered path. These are engine defaults
(`oxiroute_epoll_h1::Splice`), not configuration keys. When no pipe can be leased, the body of that response takes the
buffered path and the engine reports it in the outcome of the exchange (`Outcome::splice`): `PoolFull` when the thread
already holds its 64 pipes, `OpenFailed` when the kernel refuses a new one (no free descriptors, or the per-user
pipe memory limit `pipe-user-pages-soft` makes `F_SETPIPE_SZ` fail). `Spliced` marks a spliced body and `Unused` a
response that was not eligible. A pipe size the kernel refuses at start refuses to start the plane.

The `io_uring` plane needs kernel 6.1 (5.19 with cooperative task running), `kernel.io_uring_disabled` of 0 and a policy
that allows the `io_uring` system calls. It creates one ring per engine thread: a 2048-entry submission queue and two
provided-buffer pools, 256 buffers of 16 KiB and 64 of 64 KiB, which are 8 MiB of address space per thread and resident
only where a socket received into them. It registers no fixed buffers and no fixed files, and it uses no pipes, so
`pipe-user-pages-soft` does not constrain it. What bounds it is the memory the process can map (`RLIMIT_AS`, a cgroup
`memory.max`) and the kernel's own allocations, and on kernels that account the memory of the ring itself to the user
`RLIMIT_MEMLOCK`: the 2048-entry ring is 212 KiB with its two provided-buffer rings (`Config::ring_bytes`), every
engine thread and every plan-time probe holds one, all processes of the user draw on the same limit, and a closed ring
returns its share only after the kernel has torn it down. The kernel of the GitHub-hosted `ubuntu-24.04` runners (7.0
Azure, limit 8192 KiB) behaves so: it grants 38 such rings to the user and refuses the 39th with `ENOMEM`, whatever the
setup flags (a process with `CAP_IPC_LOCK` is not limited), so a host running many instances at once needs
`LimitMEMLOCK` raised in proportion to the engine threads it starts. The plan-time probe creates a ring with exactly these settings and
round-trips a receive through its buffers; when the kernel refuses the ring, the configuration is refused naming
`RLIMIT_MEMLOCK` with its value and the size of a ring, and when the process cannot map the buffers, naming the memory,
instead of the worker failing when its threads start. An omitted `http_data_plane` never selects `io_uring`; an explicit
`io_uring` that the probe or a later thread start cannot satisfy fails the start with that message.

An operator evacuation acts on a request served by the epoll plane as on any other: a request waiting on the evacuated
server is sent to another one and `--force` cancels it, and the epoll thread serving it is woken to act at once.

The plane reads an origin response the way the tokio path does: a bare line feed, a missing reason phrase, an
obsolete folded header and whitespace before the colon are accepted; an interim `1xx` response is forwarded and the
final response awaited; `Connection` options and `Upgrade` on a final response are removed with the fields they name;
a `Transfer-Encoding` that does not end in `chunked` delimits the body by the close of the origin connection. What the
tokio path refuses is a `502` there as well: conflicting `Content-Length` values, an invalid header byte, a `101` that
nobody asked for, more than 256 headers, a head over about 1 MiB. The plane is stricter in one case: a `Connection`
header that names more than four other fields is a `502`, and so is a first chunk whose size is not hexadecimal, where
the tokio path closes the connection without an answer.

A TLS listener is served by the epoll plane only when `http_data_plane = "epoll"` is set explicitly and its TLS profile
uses the rustls backend. The plane then runs the handshake, record decryption and encryption on the event-loop thread
(handshakes are not offloaded to `tls_handshake_threads`), with the profile's certificates, SNI selection, resumption,
protocol versions and client-certificate policy. A connection that negotiates `h2` or another ALPN, and every request
the plane does not serve, continues on the tokio path over the established TLS session together with the plaintext
already read, with no second handshake.

The HTTP/2 lane is the same idea for HTTP/2: a request for a fixed-response or plain proxy route (any body framing, `Expect`, access log) is
served on its stream without a session; every other HTTP/2 request takes the classic path. The option above controls
both lanes.

### Request tracing and in-flight requests

`oxiroute requests list` (`GET /api/v1/requests`) shows every request currently inside the router
with its request id, route, pool, server, phase, age and attempts, so a request that is lost can be
told from one that never arrived. The same id is in the access log (`requestId`) and in the
`x-request-id` header of the response and of every backend attempt. A request that is quiet for 60
seconds is logged once as stuck and counted in `oxiroute_http_stuck_requests_total{pool,phase}`.

Each target of a `server evacuate` or `pool evacuate` call is an event (`server_evacuation`, outcome `evacuated`
with `pool`, `server` (absent for a pool), `force` and the counts `cancelledAndRedispatched`, `forced` and
`leftRunning` by reason), published with the correlation id of the call, next to the audit entry of the request
(`server_evacuate` or `pool_evacuate`, category `control`). Version 1 of the events API reports it as `unknown`
and a request ended by `--force` is logged with the failure reason `evacuated`.

Every change of a server's health state reported by its active check is an event
(`upstream_endpoint_health`, outcome `health_changed` with `pool`, `server`, `previous`, `next`, the
failure `reason` when the server went down and `changedAtUnixMs`), next to the existing
`upstream_endpoint_ejection` and `upstream_endpoint_recovery` events for passive ejection. Version 1
of the events API reports it with the event name `unknown`; read it from `/api/v2/events` or `/api/v2/events/stream`.

| Metric | Labels | Meaning |
| --- | --- | --- |
| `oxiroute_http_inflight_requests` | `pool` | requests inside the router |
| `oxiroute_http_inflight_oldest_age_seconds` | `pool` | age of the oldest one |
| `oxiroute_http_inflight_overdue` | none | requests older than their deadline plus 5 s; should always be 0 |
| `oxiroute_http_stuck_requests_total` | `pool`, `phase` | requests quiet for 60 s, counted once |
| `oxiroute_pool_queue_depth` | `pool` | requests waiting for capacity on every wait path |
| `oxiroute_pool_queue_oldest_wait_seconds` | `pool` | wait of the oldest queued request |
| `oxiroute_pool_queue_wait_milliseconds` | `pool` | histogram of time spent waiting, 1 ms to 600 s |
| `oxiroute_server_responses_total` | `pool`, `server`, `class` | backend responses by `1xx`..`5xx` or `none` |
| `oxiroute_server_attempts_total` | `pool`, `server`, `outcome` | `ok`, `connect_failed`, `timeout`, `protocol_error`, `status_retry`, `rescued`, `client_closed`, `evacuated` |
| `oxiroute_evacuated_requests_total` | `pool`, `server`, `result` | requests an `evacuate` touched: `redispatched`, `left_running`, `forced`, `failed` (cancelled, then could not be served) |
| `oxiroute_server_request_duration_milliseconds` | `pool`, `server` | histogram, request sent to last byte |
| `oxiroute_server_header_duration_milliseconds` | `pool`, `server` | histogram, request sent to first response byte |
| `oxiroute_server_bytes_sent_total`, `oxiroute_server_bytes_received_total` | `pool`, `server` | bytes towards and from the backend |
| `oxiroute_server_max_connections` | `pool`, `server` | effective connection limit |
| `oxiroute_generation_info` | `generation`, `revision` | value 1 for the serving generation |
| `oxiroute_restart_required` | none | 1 while the file holds a setting that only a restart applies, 0 otherwise; the reasons are in `restartRequired` of `generation status` |
| `oxiroute_connection_scheduling_info` | `mode`, `age_ms` (only with `aged_per_worker`) | value 1: how the listener workers order ready connections (`connection_scheduling`; `mode` is the one in force, so `auto` reports `aged_per_worker` or `work_stealing`) |
| `oxiroute_aged_queue_tasks`, `oxiroute_aged_queue_ready`, `oxiroute_aged_queue_oldest_ready_seconds`, `oxiroute_aged_queue_age_seconds`, `oxiroute_aged_queue_polls_total`, `oxiroute_aged_queue_aged_polls_total` | none | only with `aged_per_worker`: connections owned by the worker queues, connections ready now, how long the oldest has waited, the age in force (the largest over the workers), polls served, and polls that served the oldest because it had waited that age; `age_ms` is `adaptive` unless `connection_age_ms` is set |
| `oxiroute_worker_scaling_info` | `mode` (`static` or `dynamic`) | value 1: `worker_scaling` of the serving generation |
| `oxiroute_worker_scaling_workers`, `oxiroute_worker_scaling_active_workers`, `oxiroute_worker_scaling_relocations_total` | `runtime` (the listener's service name) | only with `worker_scaling = dynamic`: the workers of the listener runtime, how many take new connections now, and how many idle keep-alive connections moved between workers since the runtime started |

The per-server histograms are exported only while at most 256 servers are configured; above that
only the counters are. Labels are limited to configured names, never a path, client or request id.

Alerts worth having: `oxiroute_http_inflight_overdue > 0`, `oxiroute_pool_available_servers == 0`,
`histogram_quantile(0.95, rate(oxiroute_pool_queue_wait_milliseconds_bucket[5m]))` above the callers'
patience, and `increase(oxiroute_http_stuck_requests_total[10m]) > 0`. `/metrics` and `/ready` are public on a configured
statistics bind; `/stats` and `/api/v1/status` require a loopback peer and the statistics Bearer
token.

Each canonical `stats.pages[]` socket is separate from those observability binds. Below its
`uri_prefix` it serves exactly the page, `snapshot.json`, `stats.csv` and the files below `assets/`
(see "Stats page API" and "Stats page assets") and returns `404` for every other path, for `/metrics`, `/ready`, `/status.json`, `/api/v1/status` and
the legacy `/stats` (a path that merely starts with the prefix, such as `/statsfoo`, is not the
page). `GET <prefix>` is a script page: the browser runs the embedded client, which polls
`snapshot.json`; a browser without script shows the table that needs no script and refreshes it
with a `<meta http-equiv="refresh">`, both inside `noscript`. `admin =
"localhost"` adds Ready/Drain/Maintenance only for loopback clients. Mutation additionally
requires a `localhost` or loopback-IP Host, a matching Origin or Origin-absent Referer, no forwarded
identity, and the active generation revision. Each page enforces its own connection cap and
downstream timeouts and appears in listener metrics. HEAD preserves GET `Content-Length` without a
body; use `disabled` for an unconditionally read-only page.

Security. The page has no authentication and shows everything the snapshot holds to anyone who reaches
the page listener: upstream addresses, listener binds, generation revisions, certificate names and
expiry, and configuration warnings, besides every counter. Bind a page to loopback or a trusted
interface, and put access control (a firewall, a VPN or an authenticating reverse proxy) in front of
any wider exposure. Responses carry a strict `Content-Security-Policy` (no inline script or style, no
other origin), `Referrer-Policy: no-referrer`, `X-Content-Type-Options: nosniff` and
`X-Frame-Options: DENY`, and the page, `snapshot.json` and `stats.csv` are `no-store`. The
administration buttons appear only for a loopback peer when `admin = "localhost"`; every other
client gets a read-only page.

### Stats page API

`GET <prefix>/snapshot.json` is the machine-readable form of the page, `GET <prefix>/stats.csv` a
HAProxy-style export and `GET <prefix>` the table that needs no script. The contract is
`contracts/stats-page.openapi.json`, generated from the snapshot types and checked by
`cargo test -p oxiroute --lib stats_page::contract`; regenerate it with
`cargo test -p oxiroute --lib stats_page::contract::tests::write_checked_artifact -- --ignored`.

- One snapshot is built per second and shared by every client (rebuilt at once after a page
  administration change), so viewers cost no more than one build per second. Poll no faster than
  `intervalHintMs` (the configured `refresh_ms`, at least 1000). Responses are `no-store`.
- The management API serves the same document at `GET /api/v1/stats` (bearer token, built per request,
  `admin` always `false`); `oxiroute top` reads it.
- `schemaVersion` (1) changes only for incompatible changes. Keys are camelCase. Counters are decimal
  strings (`"12345"`); gauges and timestamps (`...UnixMs`, `...Ms`) are numbers.
- A key is absent when the proxy does not know the value; nothing is reported as zero because it is
  unknown. Never provided: per-route request and status counters, per-listener status classes, peak
  gauges (`qmax`, `smax`), connect time, health-check duration or history, circuit-breaker state,
  server backup role and slow-start throttle. Latencies are lifetime histograms, not windows.
- `admin` is the only peer-dependent key: true for a loopback peer of a page with `admin =
  "localhost"`. The form is `POST <prefix>` with `application/x-www-form-urlencoded` fields
  `generation_revision` (= `generation.activeRevision`), `pool`, `server` and `state`
  (`ready`, `drain` or `maintenance`); `204` on success, `409` when the revision changed.

| Section | Content |
| --- | --- |
| `process` | version, uptime, state, CPU, memory, threads, descriptors, connections (active, max, accepted, rejected), bytes in/out, retries, upstream in-flight (count, oldest, overdue, stuck), `host` load and memory |
| `generation` | number, age, accepting, degraded, active/candidate/previous/disk/quarantined revision, last failure and rejection, counters, `retired` generations still draining |
| `workers` | `scaling` (`static`, `dynamic`) and, with `dynamic`, `runtimes[]`: runtime name, workers, active workers, connection relocations. The proxy keeps no per-worker load counter |
| `listeners[]` | name, protocol, bind, service, TLS profile, relay pool, state, administrative state, connections (active, max, accepted, rejected), bytes in/out, `http` outcomes and latency, `tcpRelays`, `proxyProtocol`, `cache` |
| `routes[]` | id, service, index, listeners, host and path selectors, methods, action, pool, status, max retries, `rescues`, `poolSelections` |
| `pools[]` | name, algorithm, `status` (`up`, `degraded`, `down`, `empty`), available/total servers, selections, `queue` (current, per class, oldest wait, totals, timeout, wait histogram), `inflight`, summed `traffic` and `latency`, `canary`, `servers[]` |
| `pools[].servers[]` | name, address, origin, `status` (`up`, `down`, `drain`, `maint`, `ejected`, `unchecked`, `unknown`), available, administrative state, health override, health, weight, connection limits, last transition, downtime, `check` (counters, last check, last failure), `passive` ejection, `traffic` (sessions, bytes, responses `1xx`..`5xx`/`none`, attempts by outcome, evacuations), `latency` (request, header) |
| `certificates[]`, `certificateWatchers[]` | name, source, expiry (`expiresAt`, `notAfterUnixSeconds`), last outcome and error, watcher health |
| `warnings[]` | configuration warnings (`W_...` code and message) |

Histograms are `{count, sumMs, buckets}` with cumulative `buckets[i]` for the i-th bound of
`bucketBoundsMs.operation` or `.queueWait`; the overflow bucket is `count`. A snapshot with about 1000
servers is 1.1 MB, sent uncompressed: with that many servers raise the viewers' refresh interval (the
`Refresh every` control) or `refresh_ms`.

The CSV header is `# pxname,svname,qcur,scur,slim,stot,bin,bout,ereq,econ,eresp,wretr,status,weight,
chkfail,lastchg,downtime,type,check_status,hrsp_1xx..hrsp_5xx,hrsp_other,req_tot,qtime,rtime,ttime`
with `type` 0 for a listener (`svname` `FRONTEND`), 1 for a pool (`BACKEND`) and 2 for a server.
Unknown cells are empty; `econ` is failed connects, `eresp` timeouts and protocol errors, `wretr`
status retries and rescues, the `*time` columns are lifetime averages in ms and `downtime` is the
current outage in seconds. Cells starting with `=`, `+`, `-` or `@` get a leading `'`.

### Stats page assets

The page's script and style files are compiled into the binary (a source build or the package
needs no node), so the page works without `management.ui_dir` or any file on disk.

- `GET <prefix>/assets/<name>` serves one embedded file; only a name that is in the table answers,
  anything else (`assets/`, nested or encoded paths, `..`) is `404` without touching the file system.
  `GET` and `HEAD` only; `HEAD` carries the `GET` headers and `Content-Length`.
- Every file name ends in a content hash (`app-Bx3k9aZ1.js`), so assets are sent with
  `Cache-Control: public, max-age=31536000, immutable` and a strong `ETag`; a matching
  `If-None-Match` gets `304`. The page, `snapshot.json` and `stats.csv` stay `no-store`.
  Assets need no snapshot, so they are served even before a generation is active.
- The page is a static shell with no inline script or style. The server fills in `<prefix>`, the
  refresh interval and the table that needs no script; the shell loads its files from
  `<prefix>/assets/`. Every response carries the policy `default-src 'none'; script-src 'self';
  style-src 'self'; connect-src 'self'; img-src 'self' data:; media-src 'self'; font-src 'self';
  form-action 'self'; base-uri 'none'; frame-ancestors 'none'`.

The files live in `crates/oxiroute-server/assets/stats/` (`index.html` and `assets/<name>`) and are
the committed output of the UI build, so the real bundle is in the repository and `cargo build` needs no
node. The single source is `ui/stats.html`, built with the asset base `/__stats_prefix__/` and installed
as `assets/stats/index.html`; the server replaces the token with the configured `uri_prefix` for each
request. To rebuild after changing the UI:

```sh
cd ui && pnpm install --frozen-lockfile && pnpm build:stats
cd .. && scripts/generate/stats/assets.sh --from ui/dist-stats
```

`scripts/generate/stats/assets.sh` installs the build and regenerates the table
`src/observability/stats_page/assets/table.rs`; with `--check` it changes nothing and fails on any
difference. `scripts/verify/stats/assets.sh` runs that check, rebuilds the UI with `pnpm build:stats`
and compares it with the embedded files byte for byte (CI runs it in the `stats-ui` job). `pnpm
build:stats` also fails when the bundle contains a virtual DOM renderer, an unexpected file, or exceeds
its size budget.

Cost. One snapshot build per second is shared by every viewer of a page, however many there are. The
measured cost of a build and the page weight are recorded in the release notes, filled in from the
benchmark run by the release notes owner.

Changing the mode of an active Unix listener is a live reload like any other: the socket keeps its
path, its inode and the connections it accepted, and the new mode is applied to the socket in place
when the candidate is prepared, so a client that connects after that sees it. A candidate that is
turned down (it cannot bind another listener, or the replacement worker refuses it) gives the
socket the mode it had before. Unix listeners retain an exclusive
`<socket>.oxiroute.lock` ownership marker so abnormal termination can safely reclaim unchanged
stale sockets that reject connection attempts. Permission-denied sockets fail closed because their
liveness cannot be proven.
Every other listener change is a live reload in both runtimes, including adding, removing, moving
and re-protocoling traffic, management, statistics, statistics-page, UDP, and HTTP/3 listeners, and
changing a listener's TLS profile. Since 0.5.3 a supervised reload no longer retains the old
revision for a listener change.

### Settings that wait for a restart

A reload applies the whole file except for the settings no reload of the running runtime can change.
Those settings keep the value the process runs with, the rest of the file is applied as a new
generation, and the process reports each waiting setting as one entry with a stable `code`, the `path`
of the setting and a one-line `summary`:

| `code` | `path` | Runtime | Why it waits |
| --- | --- | --- | --- |
| `listener_ipv6_only` | `/config/listeners/<name>/bind/ipv6_only` | both | `IPV6_V6ONLY` is fixed once the socket is bound |
| `registry_settings` | `/config/registry` | direct | the direct runtime opens the registry once |

The applied generation's revision is the revision of the file with those settings left as they run, so
`activeRevision` differs from the file's `candidateRevision` while a restart is required; editing the
setting back to its running value makes them equal again and clears the report. A restart applies the
file completely.

The report has the same shape everywhere (`restartRequired`: `required`, `candidateRevision` of the
file, `since` in Unix milliseconds, and `reasons`). `generation status`, `GET /api/v1/status`,
`GET /api/v1/generations` and the `generation reload` response carry it; `config validate` and `config
apply` list the same reasons as `restartReasons` and as `I_RESTART_REQUIRED` diagnostics with the same
`path` and `summary`. The file watcher, `SIGHUP` and `generation reload` all log it once, at warning
level on target `oxiroute::config`. A waiting setting is not a failure: nothing is quarantined, the
generation is not `degraded`, `/ready` is unaffected, and `oxiroute check` reports a warning until the
restart. The table output of `oxiroute status` and `oxiroute generation status` starts with a
`RESTART REQUIRED: <code>: <summary>` row (`restart_required` in plain output), and the gauge
`oxiroute_restart_required` is 1 while the report is required.

### Server registration

Pools with `registration` take servers at runtime. The registry keeps them in
`registry.state_dir/registrations.json` (atomic writes, a `.bak` copy, an exclusive lock); a lease is
an in-memory deadline, so every registration gets a full TTL when a process starts and heartbeats
write nothing. In supervised mode the active worker persists and expires; every worker, including
a draining one, follows the store, and a retired worker answers writes with `409 generation_inactive`
and closes the connection so clients reconnect. The group sockets are bound privately and renamed
into place, so a replacement worker takes over a path atomically; keep their directory owned by
the service user (a setgid runtime directory gives the group). If the store cannot be written the
registry reports `registry_unavailable`, keeps serving the members it has and retries.
`oxiroute_registry_*` metrics cover members by state, operations, expiries, refusals and the lease
time left of each server, and joins, changes, departures and expiries are `server_update` events.

### Listener changes on reload

A reload compares the sockets of the running revision with those of the new one. A socket is
identified by its transport and bind address (for a Unix socket, its path), not by the listener's
name, position, protocol or Unix mode.

- **Kept**: the same bind address. The socket is never closed or re-bound, so connections already
  queued on it are accepted by the new revision. A kept Unix socket takes a changed `mode` in place. A kept listener may change its protocol, service,
  TLS profile, limits and timeouts at the same time; the new revision applies them to every new
  connection while connections accepted earlier finish under the old revision.
- **Added**: a new bind address. It is bound before any traffic moves to the new revision.
- **Removed**: a bind address the new revision no longer lists. Once the new revision is active the
  socket stops accepting (new connections are refused and the address is free to bind again) while
  the requests, streams, WebSocket and relay connections it already accepted keep running in the
  old worker until they finish.
- **Moved**: a changed address is an add plus a remove.

If the new revision lists an address that cannot be bound (in use, permission denied, or a Unix
path owned by another process) the reload is rejected as a whole. The running revision, its
listeners and its connections are untouched, the master logs one warning that names the listener
(`supervised master kept revision ... because the configuration reload could not bind its
listeners`), and the same revision is tried again at the next reconciliation (at most 30 seconds
later, or immediately on `SIGHUP`, which is what `systemctl reload oxiroute` sends). The configuration API reports
such an address when it validates the candidate, so a failed bind normally surfaces before the file
is written.

The configuration API describes the effect of a change before and after saving through the
`I_LISTENER_CHANGES` diagnostic (path `/config/listeners`), which lists the `added`, `removed` and
`kept` listeners as `name (transport address)`.

A replaced generation (a worker in supervised mode) closes its idle HTTP/1 keep-alive connections
when the next revision is applied, so an idle client never holds up an apply. A request still in
progress finishes, its response carries `Connection: close`, and the client reconnects to the new
generation. Other work (a long request, a WebSocket, a CONNECT tunnel, an L4 relay, an RTMP session,
an HTTP/3 request) continues until it ends or the generation's drain deadline passes, then the
generation is closed. An HTTP/3 request that arrives on an open connection of a replaced generation is
refused with `H3_REQUEST_REJECTED`, so the client sends it again on the new generation. The deadline is, in
order, `retirement.drain_deadline_ms` of the replaced generation's own configuration, the deprecated `OXIROUTE_RELOAD_DRAIN_SECONDS` (1 to 86400) in
`/etc/oxiroute/oxiroute.env`, or the larger of 3 hours and the longest HTTP
`upstream_io_timeout_ms`; an HTTP, forward proxy or L4 service `drain_deadline_ms` only raises it.
`retirement.max_retired_generations` (default 4) bounds how many replaced generations drain at
once in the direct runtime: a further apply stays prepared (`pendingApply` with reason
`retired_cap` in `generation status`, a `generation_retire` event with outcome `cap_reached`) until
a slot frees. In supervised mode one replaced worker drains at a time, whatever the setting, so a
second reload requested while a long request keeps it alive waits for it (the master applies the
newest saved revision afterwards without further action). Until then `generation status` shows it as
`pendingApply` with reason `retired_draining`, the revision the master will apply and the connections
the draining worker still holds (`connections`); the entry disappears when the apply starts. At the deadline the retired worker
closes its generation and logs a `generation_retire` event with outcome `forced_close`; the master
kills the worker 10 seconds later if it is still there. `generation status` and
`GET /api/v1/generations` list the draining generations as `retiredGenerations` (revision, state,
age, seconds before the deadline, in-flight work by protocol). Under the supervising master the
retired worker is listed by the active worker from what the master reports once a second, and its
in-flight work is the connection total (`inFlightByKind` has a single `connections` entry). The
metrics `oxiroute_generation_retired`, `oxiroute_generation_retired_age_seconds`,
`oxiroute_generation_in_flight{role}` and `oxiroute_generation_forced_close_total{reason}` count
them. Stopping the service (`TimeoutStopSec=15s`) still ends long jobs.

Listener count. The supervising master hands the listener sockets (traffic, management, statistics and
statistics-page listeners together) to a worker in one `SCM_RIGHTS` message, so a configuration may have at
most 253 of them, the kernel's limit for one message. A start or a reload above that fails with an error naming
the limit: with the packaged launcher installed the service never falls back to the direct runtime on its own
(without the launcher, as in a development build, it runs the direct runtime, which has no such limit).

One case still needs a process restart: a TCP socket that keeps its address and changes `ipv6_only`.
`IPV6_V6ONLY` cannot change once a socket is bound, so a reload would have to bind a second `[::]`
socket beside the running one. Linux refuses that unless every socket on the port was bound with
`SO_REUSEPORT`, which listeners deliberately do not use so that a second instance on the same
address fails loudly. Even when both sockets share the port, the kernel keeps handing every new
connection to the older socket until it is shut down, and shutting a listening socket down resets
the connections still waiting in its accept queue; the direct runtime has no point at which it
could close the old socket while its generation drains. The change therefore stays startup-only,
but it does not hold the rest of the candidate back: the reload keeps the running `ipv6_only`,
applies every other change, and `restartRequired` lists the setting (`listener_ipv6_only`, see
"Settings that wait for a restart"). To change the family of a port without a restart, add a new
listener on another address first. `ListenerReservations::pending_startup_listener_changes` lists
these changes (listener name, bind, code `listener_ipv6_only`) for the reload reason model.

### Stores one worker writes

A disk cache store (`cache_stores` with `type = "disk"`) and a managed ACME state root
(`acme_managed` certificate) are written by one generation at a time, and neither blocks a reload.
Preparing the replacement validates them without locking, and the lock moves when the replacement
activates: the replaced worker keeps serving cache hits and its certificates but stops inserting,
evicting, purging, renewing and issuing, and the replacement locks the root, scans the cache (so the warm
entries survive) or reloads the certificate state, and takes over renewal. A changed cache limit
applies at that scan, and a changed `root_directory` or `state_root` is a different store: the old
one is released. While the lock is in transit the replaced worker stores no new cache entries, so a
reload costs the misses of that moment, not the cache. If the replacement cannot take the lock the
reload is rejected, the active worker takes it back and the log names the store, for example
`disk cache store `edge` at `/var/cache/oxiroute/edge` is owned by the active worker` or
`managed ACME certificate `www` state at `/var/lib/oxiroute/acme` is owned by the active worker`; a
store another daemon owns is not shared. In the direct runtime the same hand-over runs between the
two generations of the process.
The socket directory must be owned by the effective service user or be sticky, and no path ancestor
may be group/world writable unless it has the sticky bit.

### State that survives a reload

A reload replaces the generation: in the direct runtime inside one process, under the supervising master in a
new worker process. What the new generation starts with:

| State | Direct runtime | Supervised worker |
| --- | --- | --- |
| Memory cache store (`cache_stores` with `type = "memory"`) | taken by the new generation at activation, under its own bounds: a lower `max_entries` or `max_bytes` keeps the most recently used representations that fit, an object over a lower `max_object_bytes` is left out; one `memory cache store ... kept N of M entries across the reload` line per store | starts empty: a worker is another process and the cache cannot cross it; use a disk store (above) when the cache must survive a reload |
| Disk cache store | see "Stores one worker writes" | see "Stores one worker writes" |
| TLS session tickets (`session_tickets = true`, rustls backend) | one set of ticket keys per profile for the life of the process, so a ticket issued before the reload resumes after it | the replaced worker hands its keys (current, previous, next rotation time) to its replacement over the control channel before the replacement takes traffic |
| TLS session cache (`session_cache`, rustls backend) | the new generation takes the cached sessions, within a changed capacity | starts empty; tickets resume instead |
| OpenSSL backend (`policy.backend = "openssl"`) tickets and cache | OpenSSL keeps them per context, so they start empty | start empty |
| Media ticket key (`/api/v1/rtmp/media/.../ticket`, `/vod/.../ticket`) | one key for the life of the process | handed to the replacement worker like the ticket keys |
| Registered servers and their lease deadlines, operator drain, weight and checks | kept (registry) | kept (registry store) |
| Server health state, passive ejections, circuit breaker state | carried by server name to every server whose endpoint did not change: the health state, the last check time, the failure counts, the ejection with its end time and backoff step, an open or half-open breaker | the same, handed to the replacement worker; a pool too large for the control channel (48 KiB with everything else) hands its servers that are unhealthy, ejected or failing first and the rest in order, and the log names how many were left out |
| Health checks | the new generation starts checking when it is published, each server when its interval since its last check is over, so a reload does not probe every server at once; a server that is new or changed is checked at once | the same |
| Totals of checks and passive failures, the breaker's failure-ratio window, the latency averages of outlier detection | start again and refill within one window or interval | start again |
| Upstream keep-alive connections, DNS lookups of `on_connect` servers | start again | start again |
| Listener and metric counters, in-flight work | per generation: a counter starts at 0 in the new generation and the replaced one finishes its own connections | per worker |
| Active-connection counts used by `least_conn` | counted by the generation that holds the connections | the same |

A server whose endpoint changed (another address under the same name) and a pool or server that is new starts as
a fresh start does. An ejection that waited for a readmission probe ends with its quiet period instead of a probe
when it moves to the new generation. Nothing here needs a restart, so none of it is part of `restartRequired`.

## Environment variables

Settings live in the configuration. The environment variables that remain are fallbacks that a
configuration key overrides (`OXIROUTE_MANAGEMENT_TOKEN_FILE` for `management.token_file`,
`OXIROUTE_AUDIT_DIR` and `OXIROUTE_AUDIT_MAX_*` for the `audit` block, `RUST_LOG` for `log_filter`,
and the deprecated `OXIROUTE_RELOAD_DRAIN_SECONDS` for `retirement.drain_deadline_ms`), client-side
settings (`OXIROUTE_ENDPOINT` and the CLI token path), and process plumbing. Supervised workers run
with a cleared environment: the audit and token variables and `RUST_LOG` are forwarded to them, so the master and
every worker fall back to the same filter when a configuration sets no `log_filter`. `docs/developer/ENV_TO_CONFIG.md` lists every variable
with its reason.
