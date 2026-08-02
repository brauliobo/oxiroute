# OxiRoute 0.2.3

OxiRoute 0.2.3 keeps continuous RTMP recording active through storage-worker failures without
requiring the publisher to reconnect or the service to restart.

## Highlights

- Recover failed continuous recorder workers inside the existing publisher session with bounded,
  media-driven retries that never block publisher dispatch.
- Resume the latest eligible segment inside the current rotation interval and start a new segment
  after the interval expires.
- Preserve cumulative recorder counters, failure artifacts, and catalog transitions across worker
  generations while counting media skipped during recovery.
- Serialize recording finalization per storage root with bounded per-recorder and root-wide queues,
  keeping media workers independent from filesystem synchronization latency.
- Recognize SafeUnique sequence and publication-collision variants during resume, including compact
  SHA-256 identities for names near the 255-byte filesystem limit.

## Compatibility

- No configuration migration is required.
- Manual recorder start and stop behavior is unchanged; automatic restart applies only to continuous
  recorders.
- Previously generated maximum-length collision variants that truncated away their Unix start time
  cannot be identified as resume candidates and cause a new segment to be opened instead.
- The optional all-features build remains constrained by upstream `s2n-tls` requiring Rust 1.91;
  the supported default-feature build continues to use the declared Rust 1.87 minimum.
