# RTMP module and nginx-rtmp compatibility specification

## Reference and scope

The reference is `arut/nginx-rtmp-module` 1.1.4 at checkout `6c7719d`, licensed
BSD-2-Clause. It exposes 117 active directives in 18 command tables. `exec_block` is
commented out and is not an active directive.

OxiRoute will independently implement the RTMP protocol and equivalent behaviors in Rust.
Directly adapted BSD source, if ever used, requires its copyright notice and provenance.

Compatibility has two independent states:

- **Configuration compatibility:** key, context, arity, value grammar, inheritance, and diagnostics.
- **Runtime compatibility:** the configured behavior is enforced and has differential/interoperability tests.

Parsing a directive MUST NOT mark its runtime behavior supported. Each normalized directive
has one status: `enforced`, `parsed_not_enforced`, `source_no_op`, `source_bug`,
`deprecated`, or `platform_limited`.

The registry keeps the existing key-level `runtime_support` value for consumers that only need
the broad classification, and exposes `runtime_forms` for keys with a narrower lowered subset.
The compatibility report derives its counts from those same entries. A form is `enforced` only
when its documented normalized behavior reaches the current runtime; `disable_only` means that
an explicit disabling form is exact but the enabling behavior is not; `parsed_only` means that
the grammar is accepted without runtime lowering; `partial` is reserved for a key whose forms
have mixed outcomes. `source_no_op`, `source_bug`, `deprecated`, and `platform_limited` remain
non-enforced classifications. Consequently, the report may identify enforced forms without
claiming that all 117 directive keys are enforced.

The current runtime contract is intentionally narrower than the directive inventory. Bounded live
publish/play, ordered application network ACLs, stream-query tokens, per-application session
ceilings, fanout, bounded push/pull relay, bounded HTTP notify callbacks, canonical named
continuous/manual recorders, legacy AVC/AAC FLV output, bounded HLS AVC/AAC transmuxing, bounded
DASH AVC/AAC fragmented MP4 output, and allowlisted no-shell exec/transcode profiles are partial
product capabilities. The live adapter enforces a fixed 1 MiB inbound chunk ceiling, a bounded
service-configured assembled inbound-message ceiling (8 MiB maximum), and the configured RTMP
acknowledgement window. Canonical services default to an 8 MiB message ceiling and a 5,000,000-byte
acknowledgement window; imported nginx services retain nginx's 1 MiB `max_message` default and
inherit bounded `max_message` and `ack_window` values from the RTMP or server scope. Broader exec
directive parity remains partial;
broad nginx-RTMP parity remains partial. The authenticated management API provides
bounded RTMP statistics and session controls without exposing stream queries, credentials, or
private paths. Native `rtmp_stat`/`rtmp_control` directives remain classified and blocked because
their HTTP-location and XML/form semantics do not have an exact canonical route contract. Native
nginx DASH directives remain inventory-only; canonical `dash` policies use the bounded runtime path
described below.

## Context abbreviations and common values

| Code | Context |
| --- | --- |
| `N` | nginx top level |
| `R` | directly inside `rtmp {}` |
| `S` | RTMP `server {}` |
| `A` | RTMP `application name {}` |
| `C` | RTMP `recorder name {}` |
| `H` | nginx HTTP main/server/location |

Common value grammars:

- `flag`: `on | off`.
- `duration`: nginx time syntax at millisecond resolution, including `ms`, `s`, `m`, `h`.
- `size`: nginx size syntax with supported binary suffixes.
- `int`: decimal integer.
- `path`, `string`, `URL`: one post-quote/escape token unless the table says otherwise.
- `command+`: executable followed by zero or more argument/redirection tokens; no shell.
- Repeated directives retain declaration order.

Lossless import stores raw tokens beside normalized values and source locations. A safe
implementation reports reference bugs instead of reproducing memory corruption.

## Complete directive inventory

### Entry and core: 18

| Directive | Context | Accepted value | Effective default |
| --- | --- | --- | --- |
| `rtmp` | N | block, no arguments | absent |
| `server` | R | block, no arguments | repeatable |
| `listen` | S | address plus at most one of `bind`, `ipv6only=on|off`, `so_keepalive=on|off|keepidle:keepintvl:keepcnt`, `proxy_protocol` | required per server |
| `application` | S | named block | repeatable |
| `so_keepalive` | R,S | flag | off; deprecated ineffective form, use `listen` parameter |
| `timeout` | R,S | duration | 60s |
| `ping` | R,S | duration; zero disables | 60s |
| `ping_timeout` | R,S | duration | 30s |
| `max_streams` | R,S | int | 32 chunk-stream states, not RTMP message streams |
| `ack_window` | R,S | int bytes | 5,000,000 |
| `chunk_size` | R,S | int bytes | 4096 outbound after connect |
| `max_message` | R,S | size | 1M |
| `out_queue` | R,S | size parsed as message count | 256 |
| `out_cork` | R,S | size parsed as message count | `out_queue / 8`, normally 32 |
| `busy` | R,S | flag | off |
| `play_time_fix` | R,S,A | flag | on; application syntax mutates server scope in reference |
| `publish_time_fix` | R,S,A | flag | on; application syntax mutates server scope in reference |
| `buflen` | R,S | duration | 1s |

`listen` uses an nginx arity declaration that permits only one optional parameter even
though its parser loops over options. OxiRoute accepts the observable one-option grammar in
strict compatibility mode and MAY offer a canonical multi-option form.

### Access and codec: 3

| Directive | Context | Accepted value | Default/semantics |
| --- | --- | --- | --- |
| `allow` | R,S,A | `[publish|play] <CIDR|all>` or `<CIDR|all>` | ordered first match; implicit allow |
| `deny` | R,S,A | `[publish|play] <CIDR|all>` or `<CIDR|all>` | ordered first match; implicit allow |
| `meta` | R,S,A | `off | on | copy` | `on`; normalize metadata, or retain publisher payload for `copy` |

IPv4 and IPv6 CIDRs are supported. Native nginx rules retain ordered first-match semantics and
implicit allow. Canonical application policies use the same ordered first-match semantics, but a
nonempty policy denies an unmatched peer. Canonical tokens may require a `stream_query` parameter
for publish and play; query data never contributes to stream identity or observable stream keys.

### Live: 11

| Directive | Context | Accepted value | Effective default |
| --- | --- | --- | --- |
| `live` | R,S,A | flag | off |
| `stream_buckets` | R,S,A | intended positive int | 1024; reference setter corrupts adjacent state and is `source_bug` |
| `buffer` | R,S,A | duration | 0; any nonzero value enables output corking |
| `sync` | R,S,A | duration or `off` | 300ms |
| `interleave` | R,S,A | flag | off |
| `wait_key` | R,S,A | flag | on |
| `wait_video` | R,S,A | flag | off |
| `publish_notify` | R,S,A | flag | off |
| `play_restart` | R,S,A | flag | off |
| `idle_streams` | R,S,A | flag | on |
| `drop_idle_publisher` | R,S,A | duration or `off` | off |

OxiRoute treats `stream_buckets` as a safe integer. Compatibility diagnostics record the
reference defect but never emulate invalid writes.

### Relay: 6

| Directive | Context | Accepted value | Effective default |
| --- | --- | --- | --- |
| `push` | A | RTMP target followed by options | repeatable |
| `pull` | A | RTMP target followed by options | repeatable |
| `relay_buffer` | R,S | duration | 5s |
| `push_reconnect` | R,S,A | duration | 3s |
| `pull_reconnect` | R,S,A | duration | 3s |
| `session_relay` | R,S,A | flag | off |

Target scheme `rtmp://` is optional and port defaults to 1935. Accepted `push`/`pull`
options are `app=<string>`, `name=<string>`, `tcUrl=<string>`, `pageUrl=<string>`,
`swfUrl=<string>`, `flashVer=<string>`, `playPath=<string>`, and integer or bare
`live`, `start`, `stop`, `static`. `static` is rejected for push and requires `name` for
pull. Pull defaults are `flashVer=LNX.11,1,102,55`, numeric options zero, and non-static.

### External execution: 13

| Directive | Context | Accepted value | Effective default/scope |
| --- | --- | --- | --- |
| `exec` | R,S,A | command+ | repeatable alias of `exec_push` |
| `exec_push` | R,S,A | command+ | managed publisher child |
| `exec_pull` | R,S,A | command+ | managed while players exist |
| `exec_publish` | R,S,A | command+ | one-shot publish start |
| `exec_publish_done` | R,S,A | command+ | one-shot publish end |
| `exec_play` | R,S,A | command+ | one-shot play start |
| `exec_play_done` | R,S,A | command+ | one-shot play end |
| `exec_record_done` | R,S,A,C | command+ | one-shot record close |
| `exec_static` | R,S,A | command+ | repeatable, effective global scope |
| `respawn` | R,S,A | flag | on |
| `respawn_timeout` | R,S,A | duration | 5s, effective global scope |
| `exec_kill_signal` | R,S,A | decimal signal or symbolic signal | `KILL`, effective global scope |
| `exec_options` | R,S,A | flag | off, parse-order-sensitive and not inherited |

Symbolic signals: `HUP`, `INT`, `QUIT`, `ILL`, `ABRT`, `FPE`, `KILL`, `SEGV`, `PIPE`,
`ALRM`, `TERM`, `USR1`, `USR2`, `CHLD`, `CONT`, `STOP`, `TSTP`, `TTIN`, `TTOU`.

Substitutions use `$name` or `${name}`. Common names are `app`, `flashver`, `swfurl`,
`tcurl`, `pageurl`, `addr`; event names include `name`, `args`, `path`, `filename`,
`basename`, `dirname`, `recorder`. Redirections accepted by the reference include
`[fd]>/path`, `[fd]>>/path`, `[fd]</path`, `[fd]>&source_fd`, `[fd]<&source_fd`.

OxiRoute MUST run allowed commands through an isolated no-shell worker with executable
allowlists and resource limits. It does not reproduce reference pointer/filter defects.

Canonical `rtmp_services[].exec_profiles` entries use one exact executable path, typed argv and
environment entries, an explicit working directory, and bounded process, queue, output, timeout,
respawn, and shutdown limits. Arguments are sent directly to the operating-system process API;
shell parsing, redirection, inherited environment, loader variables, and shell interpreter
executables are rejected. `network = "disabled"` is the default and fails closed when the daemon
cannot provide a network namespace; `network = "inherited"` is an explicit less-isolated policy.
`mode = "transcode"` accepts bounded typed media frames on the worker stdin queue. Worker status
contains only redacted correlation, phase, counters, and failure categories.

### Recording: 11

| Directive | Context | Accepted value | Effective default |
| --- | --- | --- | --- |
| `record` | R,S,A,C | bitmask: `off all audio video keyframes manual` | empty/off |
| `record_path` | R,S,A,C | path | empty |
| `record_suffix` | R,S,A,C | string; strict lowering accepts bounded literals and `%%` only | `.flv` |
| `record_unique` | R,S,A,C | flag | off |
| `record_append` | R,S,A,C | flag | off |
| `record_lock` | R,S,A,C | flag | off |
| `record_max_size` | R,S,A,C | size; zero unlimited | 0 |
| `record_max_frames` | R,S,A,C | size parsed as frame count; zero unlimited | 0 |
| `record_interval` | R,S,A,C | duration | unset |
| `record_notify` | R,S,A,C | flag | off |
| `recorder` | A | named block | repeatable |

`all` means audio and video. `off` wins if combined. `keyframes` records video keyframes;
`manual` requires control API start. The strict lowerer maps only `record all`, `record all manual`,
`record manual all`, and `record off`; narrower bitmasks and bare `record manual` block finalization.
Canonical configuration and the live runtime support named recorder definitions, including
continuous and manual modes, exact-ID manual controls, bounded storage, and isolated worker
failures. Native `recorder <name>` blocks are also lowered when their effective policy is inside
the strict subset below. Explicit append/lock, nonzero per-file size/frame bounds, notify, and
other recorder forms remain unsupported; a named block with one of those forms is blocked rather
than silently approximated.

The native `record_unique` form appends segment-start Unix seconds and is not collision-free when
multiple segments start in one second. OxiRoute preserves that suffix and then uses exclusive partial creation
and atomic no-replace publication with bounded collision suffixes. Canonical configuration supports
the bounded `%Y %m %d %H %M %S %%` subset. Strict nginx lowering requires one uniquely consumed
host IANA timezone overlay and renders calendar fields from segment-start time, matching the native
local-time basis without inferring the host timezone.

Canonical recording percent-escapes one relative stream-name component, drops query arguments,
limits the rendered name to 255 bytes, and rotates on audio boundaries or video keyframes. These
policies are wired into configured publisher sessions and exact-ID manual controls.
For a segment-start, nginx-compatible recorder with `record_unique on` and `record_interval`, a
publisher or daemon reconnect resumes the latest exact-policy filename while its original start is
still inside that interval. OxiRoute validates the existing FLV tail, continues timestamps after its
last complete tag, and keeps rotation anchored to the original segment start. Once the interval has
expired, the reconnect creates a new nginx-style filename.

### Strict nginx-RTMP lowering

`oxiroute-import` has a separate nginx-RTMP entry point that loads deterministic includes, resolves
inheritance across `rtmp`, `server`, and `application` scopes, records one terminal disposition per
expanded occurrence, retains provenance, and conditionally finalizes a canonical RTMP config. It
does not inspect or mutate `record_path`; canonical runtime preflight remains a separate step.

The finalizable subset is intentionally narrow:

- One effective `rtmp` block with non-overlapping IP socket `listen` values and no listen options.
- `server` and uniquely named `application` blocks.
- Inheritable `live` and `idle_streams` flags.
- Ordered `allow`/`deny` network rules for publish and play.
- A finite application-scoped `max_connections` ceiling.
- `record off`, `record all`, or `record all manual`/`record manual all` on a live application.
- A required secure absolute `record_path` when recording is enabled.
- Default `.flv` or a separator-free suffix of at most 128 bytes containing only literals and `%%`.
- `record_unique on|off` and, for continuous recording only, `record_interval` from 1 through
  2147483647 milliseconds.
- Named `recorder <name>` blocks with the same exact recording policy. Their names and provenance
  are retained in the canonical recorder list.
- Canonical finite queue, shutdown, storage, file, and active-recorder defaults for imported
  recorders.
- One RTMP-scope `access_log off` or one absolute `access_log <path> [combined]` form.

Any applicable unsupported directive blocks its server; any blocking error prevents a finalized
config, although other safe servers may remain visible in the draft. Blockers include global/server
`max_connections`, listen options, overlapping listeners, duplicate scalar/application identities,
non-live recording, missing/insecure paths, local-time suffix formats, manual intervals, partial
recording bitmasks, push/pull, notify, unsupported exec forms, VOD, HLS/DASH, `log_format`, nested or non-combined
logging, stat/control behavior, enabled append/lock, nonzero size/frame limits, and named
`recorder {}` blocks with unsupported effective fields. The `import_rtmp` entry point remains a Rust library without a separate `import rtmp`
command, import API, or import UI. Complete nginx-root import does integrate the strict RTMP result:
the CLI can report/preview it, and KDL/HOCON/UCI `nginx_server` references can compose it into the
canonical resolver and watcher-driven generation path.

### Management statistics and controls

The bearer-authenticated management service exposes these bounded RTMP views:

| Route | Method | Semantics |
| --- | --- | --- |
| `/api/v1/rtmp/stats` | `GET` | global, live, and connected-client views |
| `/api/v1/rtmp/stats/global` | `GET` | aggregate stream/media counters |
| `/api/v1/rtmp/stats/live` | `GET` | at most 1,024 stream identities without queries |
| `/api/v1/rtmp/stats/clients` | `GET` | at most 1,024 connected sessions |
| `/api/v1/rtmp/clients/<session-id>/drop` | `POST` | drop the whole client session |
| `/api/v1/rtmp/clients/<session-id>/publisher/drop` | `POST` | drop only a publisher session |
| `/api/v1/rtmp/clients/<session-id>/subscriber/drop` | `POST` | drop only a subscriber session |

All revision fields are decimal strings. State-changing requests require exactly one
`If-Rtmp-Session-Revision` header. The runtime checks the session revision and current role before
queuing a disconnect; publisher catalog ownership remains session-ID qualified, so a stale request
cannot terminate a replacement publisher. Controls are polled at a bounded 100 ms interval and
return `202` when queued.

The operational event stream recognizes `rtmp_connect`, `rtmp_publish`, `rtmp_play`,
`rtmp_disconnect`, and `rtmp_access`. Enabled RTMP access logging reuses the bounded asynchronous
JSONL worker and emits only timestamp, service, session ID, application, stream name, role, client
IP, event, and outcome. Stream queries, credentials, recording roots, and private paths are never
written.

### VOD and netcall: 5

| Directive | Context | Accepted value | Effective default |
| --- | --- | --- | --- |
| `play` | R,S,A | one or more ordered local roots or `http://` bases | empty, repeatable |
| `play_temp_path` | R,S,A | path | `/tmp` |
| `play_local_path` | R,S,A | path | empty |
| `netcall_timeout` | R,S | duration | 10s |
| `netcall_buffer` | R,S | size | 1024 |

VOD infers `.flv`; `mp4:` selects `.mp4`. Query options `aindex` and `vindex` choose MP4
tracks and default to zero. Reference remote VOD supports plain HTTP only, downloads the
whole object to a temporary file, and treats `https://` as a local path. OxiRoute strict
compatibility preserves accepted grammar while secure canonical mode may provide explicit
HTTPS behavior under a distinct option.

### HTTP notifications: 13

| Directive | Context | Accepted value | Effective default |
| --- | --- | --- | --- |
| `on_connect` | R,S | HTTP URL | unset |
| `on_disconnect` | R,S | HTTP URL | unset |
| `on_publish` | R,S,A | HTTP URL | unset |
| `on_play` | R,S,A | HTTP URL | unset |
| `on_publish_done` | R,S,A | HTTP URL | unset |
| `on_play_done` | R,S,A | HTTP URL | unset |
| `on_done` | R,S,A | HTTP URL | unset |
| `on_record_done` | R,S,A,C | HTTP URL | unset |
| `on_update` | R,S,A | HTTP URL | unset |
| `notify_method` | R,S,A | `get | post` | `post` |
| `notify_update_timeout` | R,S,A | duration | 30s; zero disables |
| `notify_update_strict` | R,S,A | flag | off |
| `notify_relay_redirect` | R,S,A | flag | off |

Canonical callback policies use bounded HTTP/1.1 GET or POST requests over HTTP or HTTPS. Every
resolved address is checked against the service outbound policy and the selected address is pinned
for the callback endpoint. 2xx succeeds; non-2xx responses fail authorization or are recorded as a
non-strict notification failure. `on_connect`, `on_publish`, and `on_play` are authorization
callbacks. Done and disconnect callbacks run during role/connection teardown. `on_update` is
bounded by `notify_update_timeout`; zero disables updates and strict mode closes the session on an
update failure. Callback URLs, queries, and secrets are redacted from debug/error output.

### Logging, limits, and auto-push: 6

| Directive | Context | Accepted value | Effective default/scope |
| --- | --- | --- | --- |
| `access_log` | R,S,A | `off` or path plus optional format name | compiled log path, `combined`; repeatable |
| `log_format` | R,S,A | name plus format tokens | `combined`; effective R/global scope |
| `max_connections` | R,S,A | int | unset; effective global across workers, zero rejects all |
| `rtmp_auto_push` | N | flag | off |
| `rtmp_auto_push_reconnect` | N | duration | 100ms |
| `rtmp_socket_dir` | N | path | `/tmp` |

Log variables: `connection`, `remote_addr`, `app`, `flashver`, `swfurl`, `tcurl`,
`pageurl`, `command`, `name`, `args`, `bytes_sent`, `bytes_received`, `time_local`, `msec`,
`session_time`, `session_readable_time`. Auto-push requires Unix-domain sockets and is
platform-limited in the reference.

OxiRoute lowers the Nginx main-scope auto-push subset into the bounded
`rtmp_services[].auto_push` policy. Enabled workers create owner-only Unix sockets lazily on
first publisher admission. A shared owner-only secret authenticates framed worker handshakes;
the frame carries only the service, stream identity, publisher session/incarnation token, sequence,
timestamp, event kind, and bounded media payload. Peer copies register as local-only publishers,
never start configured relays/recorders/exec/media outputs, and never forward received frames.
Queue overflow drops until a bounded metadata/header/keyframe bootstrap is available. Worker,
session, incarnation, sequence, peer, stream, frame, authentication, and queue limits are validated
before activation, with unsupported non-Unix and unsafe socket/credential paths failing closed.

### HLS: 22

| Directive | Context | Accepted value | Effective default |
| --- | --- | --- | --- |
| `hls` | R,S,A | flag | off |
| `hls_fragment` | R,S,A | duration | 5s |
| `hls_max_fragment` | R,S,A | duration | 10 x fragment, normally 50s |
| `hls_path` | R,S,A | path | empty |
| `hls_playlist_length` | R,S,A | duration | 30s |
| `hls_muxdelay` | R,S,A | duration | 700ms; parsed but reference runtime uses fixed 700ms (`source_no_op`) |
| `hls_sync` | R,S,A | duration | 2ms |
| `hls_continuous` | R,S,A | flag | on |
| `hls_nested` | R,S,A | flag | off |
| `hls_fragment_naming` | R,S,A | `sequential | timestamp | system` | `sequential` |
| `hls_fragment_slicing` | R,S,A | `plain | aligned` | `plain` |
| `hls_type` | R,S,A | `live | event` | `live` |
| `hls_max_audio_delay` | R,S,A | duration | 300ms |
| `hls_audio_buffer_size` | R,S,A | size | 1M |
| `hls_cleanup` | R,S,A | flag | on |
| `hls_variant` | R,S,A | stream suffix plus raw master-playlist attributes | repeatable |
| `hls_base_url` | R,S,A | string | empty |
| `hls_fragment_naming_granularity` | R,S,A | int; zero disables | 0 |
| `hls_keys` | R,S,A | flag | off |
| `hls_key_path` | R,S,A | path | `hls_path` |
| `hls_key_url` | R,S,A | string | empty |
| `hls_fragments_per_key` | R,S,A | int; zero means one key | 0 |

The canonical runtime supports H.264/AVC video and AAC audio in MPEG-TS. Keys are random 128-bit
AES-128-CBC material. The nginx importer lowers the bounded `hls`, `hls_path`, fragment, playlist,
nested, cleanup, naming, and key-rotation subset; `hls_variant`, custom key paths, and other
packaging forms remain blocked until their canonical semantics are available. Canonical HLS variants
are validated before being rendered into HTTP output.

### MPEG-DASH: 6

| Directive | Context | Accepted value | Effective default |
| --- | --- | --- | --- |
| `dash` | R,S,A | flag | off |
| `dash_fragment` | R,S,A | duration | 5s |
| `dash_path` | R,S,A | path | empty |
| `dash_playlist_length` | R,S,A | duration | 30s |
| `dash_cleanup` | R,S,A | flag | on |
| `dash_nested` | R,S,A | flag | off |

Canonical DASH policies accept validated AVC/AAC input only. They write an ISO-BMFF initialization
segment, independent keyframe-aligned `moof`/`mdat` media segments, and a bounded MPD
`SegmentList`; segment files and manifests are published atomically under the configured media
quota. `segment_naming`, `max_segment_duration_ms`, `max_segment_bytes`, queue/storage quotas,
nesting, cleanup, and restart continuity are canonical fields. Enhanced AVC, HEVC, AV1, malformed
codec records, unsafe names, and native nginx-DASH lowering remain explicitly blocked.

### HTTP statistics and control: 3

| Directive | Context | Accepted value | Effective default |
| --- | --- | --- | --- |
| `rtmp_stat` | H | bitmask `all global live clients` | 0 |
| `rtmp_stat_stylesheet` | H | string | empty |
| `rtmp_control` | H | bitmask `all record drop redirect` | 0 |

`rtmp_stat all` includes an internal play bit, but literal `play` is rejected. Statistics
are XML in the reference; the stylesheet directive only inserts a reference.

Control URI shape is `<location>/<section>/<method>?<filters>`:

- `record/start|stop`: publisher filters `srv`, `app`, `name`, `addr`, `clientid`, `rec`.
- `drop/publisher|subscriber|client`: session filters `srv`, `app`, `name`, `addr`, `clientid`.
- `redirect/publisher|subscriber|client`: same filters plus required `newname`.

The reference control endpoint has no intrinsic authorization. OxiRoute management
authentication and per-action authorization apply before any control operation.

## Alphabetical completeness list

```text
access_log, ack_window, allow, application, buffer, buflen, busy, chunk_size,
dash, dash_cleanup, dash_fragment, dash_nested, dash_path, dash_playlist_length,
deny, drop_idle_publisher, exec, exec_kill_signal, exec_options, exec_play,
exec_play_done, exec_publish, exec_publish_done, exec_pull, exec_push,
exec_record_done, exec_static, hls, hls_audio_buffer_size, hls_base_url,
hls_cleanup, hls_continuous, hls_fragment, hls_fragment_naming,
hls_fragment_naming_granularity, hls_fragment_slicing, hls_fragments_per_key,
hls_key_path, hls_key_url, hls_keys, hls_max_audio_delay, hls_max_fragment,
hls_muxdelay, hls_nested, hls_path, hls_playlist_length, hls_sync, hls_type,
hls_variant, idle_streams, interleave, listen, live, log_format, max_connections,
max_message, max_streams, meta, netcall_buffer, netcall_timeout, notify_method,
notify_relay_redirect, notify_update_strict, notify_update_timeout, on_connect,
on_disconnect, on_done, on_play, on_play_done, on_publish, on_publish_done,
on_record_done, on_update, out_cork, out_queue, ping, ping_timeout, play,
play_local_path, play_restart, play_temp_path, play_time_fix, publish_notify,
publish_time_fix, pull, pull_reconnect, push, push_reconnect, record,
record_append, record_interval, record_lock, record_max_frames, record_max_size,
record_notify, record_path, record_suffix, record_unique, recorder, relay_buffer,
respawn, respawn_timeout, rtmp, rtmp_auto_push, rtmp_auto_push_reconnect,
rtmp_control, rtmp_socket_dir, rtmp_stat, rtmp_stat_stylesheet, server,
session_relay, so_keepalive, stream_buckets, sync, timeout, wait_key, wait_video
```

Group count check: `1 + 17 + 2 + 1 + 11 + 6 + 13 + 11 + 3 + 2 + 13 + 2 + 1 + 3 + 22 + 6 + 2 + 1 = 117`.

## Runtime architecture

The target Rust module is layered as follows; the live path and canonical recording pipeline
described below are implemented, not evidence that every listed component exists:

1. Listener and optional PROXY protocol.
2. RTMP version-3 simple/complex handshake state machine.
3. Incremental chunk decoder/encoder for formats 0-3, CSID forms, extended timestamps, reassembly,
   fixed inbound chunk/message limits, ACK, bandwidth, and ping. The current message ceiling is
   fixed by the session adapter rather than configured per native directive.
4. Bounded AMF0 codec and command decoder.
5. Ordered command middleware for connect, createStream, publish, play, closeStream, and deleteStream.
6. Application stream registry with one publisher, subscribers, cached metadata/AAC/AVC headers, and keyframe state.
7. Bounded per-subscriber output queues with priority/drop/resynchronization policy.
8. Independent relay, record, VOD, callback, segmenter, exec, auto-push, stats, and control
   components. Auto-push is a framed local-worker transport, not an arbitrary RTMP endpoint.

The first compatibility mode uses one RTMP message stream per connection, ID 1, matching
the reference assumption. Multi-message-stream support is a later explicit extension.

The runtime catalog publishes immutable active-stream snapshots with
restart-safe stream/session/recorder identities, publisher/subscriber counts, absolute media
samples, and configured recorder transitions. The Pingora RTMP listener accepts one live
publisher or viewer role per connection for explicitly configured applications. A bounded fanout
hub caches metadata/AAC/AVC headers, gates viewers on future keyframes, resynchronizes saturated
viewers independently, resets on publisher restart, and detaches both roles on stop, disconnect,
shutdown, or protocol failure. Playback serialization happens after queue extraction, outside hub
locks, in bounded drain turns with transport write deadlines.

Each publisher incarnation receives the configured recorder definitions. Continuous recorders enter
`starting` during publisher registration and open output on the first recordable media. Manual
recorders remain `idle` until an exact stream/recorder start request. Stop, publisher disconnect,
shutdown, or protocol failure stops admission, drains or cancels the worker, finalizes durable
segments when possible, and removes that publisher's recorder identities.

A manual start moves `idle` or `failed` to `starting`; start is idempotent while already `starting`
or `recording`. A manual stop moves `recording` to `stopping`; stop is idempotent in `idle`,
`failed`, or `stopping`. Start during `stopping` and stop during `starting` are conflicts. Worker
status advances `starting` to `recording` after output opens, and successful asynchronous stop
returns to `idle`. Continuous recorders are not manually controllable. After a worker failure, the
controller reaps the failed worker without blocking publisher media dispatch and starts a replacement
within the same publisher incarnation after a bounded retry delay and subsequent media. When
segment-start naming encodes the native Unix-second start, the replacement resumes the latest
eligible published file inside the configured interval; otherwise it starts a new file for the
current interval. Recorder counters remain cumulative across replacement workers, failure artifacts
remain observable, and media skipped during recovery is counted as dropped.

The store uses no-follow directory traversal, daemon-owned roots without group/other write bits,
exclusive hidden partials, atomic no-replace publication, startup cleanup under an ownership lease
on the descriptor-pinned root directory,
and byte/file/active-recorder limits. One lease accounts for each recorder worker's full lifetime;
segment files and pending finalizations do not consume additional recorder slots. Runtime-plan and
config-API candidate validation use a
read-only preflight; actual RTMP service activation opens and descriptor-pins the root. Existing
files count toward quota. Equal limits for one normalized
root share counters within one daemon process only; the lock protocol is cross-process, but quota
accounting is not.

Publisher threads use `try_enqueue` and never wait for queue capacity or disk I/O. If adding one
event would exceed the message or byte bound, the worker drops the queued events and triggering
event, records one discontinuity, stops accepting, and preserves the active partial; it does not
silently extend that partial after a gap. The continuous controller reaps the failed worker and
starts a clean replacement in the same publisher incarnation. Rotation waits for a video keyframe
when video has appeared, or an audio boundary for audio-only output. The worker closes the old FLV
and starts the next segment before a recording-root finalizer pool synchronizes and publishes the
old file, so durable storage latency does not stall media queue draining. Each root
uses one finalizer thread, permits at most two pending segments per recorder, and bounds the root
queue accordingly. Aligned rotations therefore create neither an unbounded thread/filesystem-sync
storm nor a media-worker wait at the following rotation; exhausting the bounded backlog fails
explicitly as finalization while preserving the current segment.

Recorder open, quota, write, discontinuity, and codec failures are isolated to that recorder. They
remain observable but do not fail live ingest, fanout, or sibling recorders.

The recorder mask is applied before queue admission. Audio, video, or keyframes-only video may be
selected, but at least one audio/video track is required. `append` resumes the exact existing FLV
segment only after descriptor, ownership, lock, and FLV-tail checks; `lock` holds an exclusive
advisory lock for the active segment. `max_size` and `max_frames` are per-segment bounds. A reached
bound rotates at the next safe audio boundary or video keyframe; if no safe boundary arrives, the
worker drops over-limit non-keyframes rather than exceeding the bound. `notify` retains the latest
bounded lifecycle outcome in the recorder status.

Applications may also declare named VOD sources. A VOD RTMP play name has the form
`source/relative/path.flv`; it is admitted only when the application has a configured source and a
viewer lease is available. Local objects are opened from a pinned no-follow root. HTTP objects are
fetched by a dedicated bounded worker after origin policy and DNS-answer validation. The worker
owns the VOD lease through playback, parses only bounded FLV audio/video tags, and emits
`NetStream.Play.Complete` after the bounded duration or object ends. A failed source does not block
the listener or other viewers.

The authenticated management API exposes the same source as
`GET /api/v1/rtmp/vod/{service}/{application}/{source}/{path}`. It supports one contiguous byte
range, rejects multiple ranges and chunked upstream responses, and never exposes source roots or
origin credentials.

HLS and DASH output are configured per live application and served through the authenticated management API
at `GET /api/v1/rtmp/media/{service}/{application}/{stream}/{object}`. The media worker uses a
bounded queue and publisher-incarnation directory, publishes complete playlists/fragments by
atomic rename, enforces byte/file/active-stream quotas, and removes old playlist objects when the
configured retention window advances. Legacy AVC/AAC input is transmuxed to MPEG-TS; configured
variants share the same media bytes. Optional AES-128 keys rotate by segment count and remain inside
the bounded media root. DASH writes `init.mp4`, `.m4s` fragments, and `manifest.mpd` as real
fragmented MP4 output, retains a bounded MPD window, preserves sequence continuity across a
publisher restart, and supports authenticated single contiguous byte ranges. DASH media is never
represented as MPEG-TS.

Stop and disconnect transfer workers to a reaper with a bounded pending-task count. Submission
backpressures when that bound is full, but waits outside registry and recorder-controller locks, so
catalog snapshots and controller observation remain available. Each worker stops accepting events
and has its configured 1-60000 ms shutdown deadline; the reaper requests cancellation after that
deadline, retains ownership, and joins instead of detaching. Reaper shutdown cancels outstanding
tasks and waits for their completion.

FLV recording supports legacy AVC video and AAC audio. Enhanced RTMP AVC (`avc1`), HEVC (`hvc1`),
and AV1 (`av01`) can be observed and fanned out by the live path but are explicitly
`recording_supported: false`; a configured recorder that encounters those video events fails with
`unsupported_codec` rather than emitting misleading FLV output.

## Implementation slices and acceptance

### Slice 0: directive registry

Status: parser, registry, and contextual value validation are implemented. Deterministic include
resolution, effective inheritance, occurrence accounting, provenance, and strict lowering are
implemented for the subset above; per-directive lowering and fixture completeness remain.

- Exactly 117 unique active keys.
- Context, arity, value grammar, defaults, scope quirks, and status for every key.
- Enum/bitmask/options validation and lossless raw tokens.
- `exec_block` recognized only as inactive/unsupported.

### Slice 1: live interoperability

Status: partial. A pinned `rml_rtmp` 0.8.0 adapter now provides simple/complex handshakes, chunk
transport, connect/createStream/live-publish/play command handling, duplicate-publisher rejection,
bounds for inbound chunks and service-configured assembled messages, configured acknowledgement
windows, bounded media fanout, media observations, and lifecycle cleanup. The listener caps
requested inbound chunks at 1 MiB and canonical assembled messages at 8 MiB. Manual FFmpeg
publishing and native publish/play wire tests pass. Checked-in process-level FFmpeg consume
acceptance, exhaustive chunk fixtures, and OBS acceptance remain before this slice is complete.

- Simple and both Adobe complex handshake schemes.
- Fragmented I/O, chunk formats 0-3, all CSID header widths, extended timestamps, and interleaving.
- Dynamic inbound chunk size, max-message, ACK window, ping, and output queue bounds.
- AMF0 `connect`, `createStream`, `publish`, `play`, `closeStream`, `deleteStream`.
- One publisher and many subscribers, duplicate-publisher rejection, idle subscribers.
- Metadata/AAC/AVC header cache, future keyframe gating, queue saturation, and restart.
- OBS/FFmpeg publish and FFmpeg/ffplay consume interoperability.

### Slice 2: operational parity

Status: partial. Canonical continuous/manual recording, named recorders, live-session dispatch,
catalog completion, storage, rotation, observability, exact-ID bearer-protected controls, access and
notify policy, pull relay, bounded local/HTTP VOD, bounded HLS/DASH output, and same-daemon auto-push
are integrated for legacy AVC/AAC media. Static push relay and the first isolated exec/transcode
slice are integrated separately. Native stats-page/control parity, native access-log syntax,
enhanced codec recording, broader exec directive parity, and broad nginx-RTMP lowering remain.

### Slice 3: media/process parity

HLS, DASH, isolated exec, limits, and multi-worker equivalents require dedicated crash,
resource-exhaustion, and active-traffic evidence before parity claims can expand. The current
runtime coverage includes bounded HLS MPEG-TS output, DASH fMP4/MPD output, cleanup, quota, restart,
range, authentication, and malformed-input tests. The first exec slice has canonical profiles,
typed native publisher/publish-done lowering, bounded process admission, and an isolated lifecycle
worker; broader directive parity and production crash campaigns remain blocked.

## Security requirements

- Handshake, chunk, AMF, metadata, and URL/token parsers are size/depth/time bounded.
- RTMP names cannot escape recording, VOD, HLS, DASH, or key roots.
- Callbacks and relay targets obey outbound-origin and resolved-address policy.
- Recognized management/API routes, including recorder controls, require the management bearer
  token except exact `GET /ready` and `GET /metrics`. The management listener remains loopback-only;
  a future remote mode MUST add explicit authorization and audit policy before exposure.
- Exec profiles are allowlisted, no-shell, bounded, and fail closed when the requested isolation
  policy is unavailable.
- Per-listener, application, publisher, subscriber, message, queue, and segment limits are explicit.
- Malformed publisher input cannot produce unbounded subscriber memory or unsafe media files.

## Test strategy

Current tests cover unit state machines, loopback integration, byte-exact media output, bounded
fanout, native publish/play wire behavior, recording path/store/worker failure cases, bounded HLS/DASH
output, auto-push framing, isolated exec lifecycle, and directive context/value validation. The
checked-in fuzz harnesses currently have bounded build/smoke coverage; captured differential traces,
process-level FFmpeg/OBS consume acceptance, long-running fuzz/crash campaigns, and broader
fake-clock/filesystem matrices remain open. Every directive needs positive context/value fixtures and
negative context/arity/value fixtures before the runtime matrix can claim full config compatibility.
