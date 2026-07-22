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

IPv4 and IPv6 CIDRs are supported. Local rules precede inherited rules. Internal relays
and auto-push have special reference bypass behavior that requires explicit tests.

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

### Recording: 11

| Directive | Context | Accepted value | Effective default |
| --- | --- | --- | --- |
| `record` | R,S,A,C | bitmask: `off all audio video keyframes manual` | empty/off |
| `record_path` | R,S,A,C | path | empty |
| `record_suffix` | R,S,A,C | string, `%` enables `strftime` | `.flv` |
| `record_unique` | R,S,A,C | flag | off |
| `record_append` | R,S,A,C | flag | off |
| `record_lock` | R,S,A,C | flag | off |
| `record_max_size` | R,S,A,C | size; zero unlimited | 0 |
| `record_max_frames` | R,S,A,C | size parsed as frame count; zero unlimited | 0 |
| `record_interval` | R,S,A,C | duration | unset |
| `record_notify` | R,S,A,C | flag | off |
| `recorder` | A | named block | repeatable |

`all` means audio and video. `off` wins if combined. `keyframes` records video keyframes;
`manual` requires control API start. FLV append, locking, interval split, and codec-header
rules require byte-level compatibility tests.

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

Reference callbacks use plain HTTP/1.0; 2xx succeeds, 3xx may rename or relay, other results
fail. Common form fields are `app`, `flashver`, `swfurl`, `tcurl`, `pageurl`, `addr`,
`clientid`, plus event fields and original stream query arguments. `notify_method` has a
reference cross-application scope quirk that is diagnosed rather than accidentally shared.

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

Media compatibility is H.264/AVC video and AAC audio in MPEG-TS. Keys are random 128-bit
AES-128-CBC material. Playlist attributes from `hls_variant` require validation before
being rendered into HTTP output.

### MPEG-DASH: 6

| Directive | Context | Accepted value | Effective default |
| --- | --- | --- | --- |
| `dash` | R,S,A | flag | off |
| `dash_fragment` | R,S,A | duration | 5s |
| `dash_path` | R,S,A | path | empty |
| `dash_playlist_length` | R,S,A | duration | 30s |
| `dash_cleanup` | R,S,A | flag | on |
| `dash_nested` | R,S,A | flag | off |

Media compatibility is H.264/AVC and AAC in fragmented MP4 with dynamic MPD output.

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

The Rust module is layered:

1. Listener and optional PROXY protocol.
2. RTMP version-3 simple/complex handshake state machine.
3. Incremental chunk decoder/encoder for formats 0-3, CSID forms, extended timestamps, reassembly, size limits, ACK, bandwidth, and ping.
4. Bounded AMF0 codec and command decoder.
5. Ordered command middleware for connect, createStream, publish, play, closeStream, and deleteStream.
6. Application stream registry with one publisher, subscribers, cached metadata/AAC/AVC headers, and keyframe state.
7. Bounded per-subscriber output queues with priority/drop/resynchronization policy.
8. Independent relay, record, VOD, callback, segmenter, exec, stats, and control components.

The first compatibility mode uses one RTMP message stream per connection, ID 1, matching
the reference assumption. Multi-message-stream support is a later explicit extension.

The runtime-neutral catalog already publishes immutable active-stream snapshots with
restart-safe stream/session/recorder identities, publisher/subscriber counts, absolute media
samples, and capability-gated recorder transitions. The Pingora RTMP listener now attaches a
publisher only after accepting a live `publish` request, rejects duplicate publishers, observes
media statistics, and detaches on stop, disconnect, shutdown, or protocol failure. Handshakes and
connections alone never create catalog streams. Playback remains rejected until bounded fanout is
implemented.

## Implementation slices and acceptance

### Slice 0: directive registry

Status: parser, registry, and contextual value validation implemented; include resolution,
effective inheritance lowering, and per-directive fixture completeness remain.

- Exactly 117 unique active keys.
- Context, arity, value grammar, defaults, scope quirks, and status for every key.
- Enum/bitmask/options validation and lossless raw tokens.
- `exec_block` recognized only as inactive/unsupported.

### Slice 1: live interoperability

Status: partial. A pinned `rml_rtmp` 0.8.0 adapter now provides simple/complex handshakes, chunk
transport, connect/createStream/live-publish command handling, duplicate-publisher rejection,
media observations, and lifecycle cleanup. The listener caps requested inbound chunks at 1 MiB.
Play/fanout, configurable assembled-message limits, exhaustive chunk fixtures, and independent
FFmpeg/OBS acceptance remain before this slice is complete.

- Simple and both Adobe complex handshake schemes.
- Fragmented I/O, chunk formats 0-3, all CSID header widths, extended timestamps, and interleaving.
- Dynamic inbound chunk size, max-message, ACK window, ping, and output queue bounds.
- AMF0 `connect`, `createStream`, `publish`, `play`, `closeStream`, `deleteStream`.
- One publisher and many subscribers, duplicate-publisher rejection, idle subscribers.
- Metadata/AAC/AVC header cache, future keyframe gating, queue saturation, and restart.
- OBS/FFmpeg publish and FFmpeg/ffplay consume interoperability.

### Slice 2: operational parity

Access, notify, push/pull, recording, VOD, stats, controls, and logs become enforced only
after their failure and security paths pass differential tests.

### Slice 3: media/process parity

HLS, DASH, isolated exec, limits, and multi-worker equivalents require dedicated storage,
media, crash, and resource-exhaustion tests.

## Security requirements

- Handshake, chunk, AMF, metadata, and URL/token parsers are size/depth/time bounded.
- RTMP names cannot escape recording, VOD, HLS, DASH, or key roots.
- Callbacks and relay targets obey outbound-origin and resolved-address policy.
- Control operations use management authentication; no nginx-compatible unauthenticated default.
- Exec is disabled until an isolated allowlisted worker exists.
- Per-listener, application, publisher, subscriber, message, queue, and segment limits are explicit.
- Malformed publisher input cannot produce unbounded subscriber memory or unsafe media files.

## Test strategy

Tests begin red and include unit state machines, loopback integration, captured golden
traces, differential behavior against the reference, fuzzing, fake clocks/filesystems, and
independent FFmpeg/OBS clients. Every directive has positive context/value fixtures and
negative context/arity/value fixtures before the runtime matrix can claim full config
compatibility.
