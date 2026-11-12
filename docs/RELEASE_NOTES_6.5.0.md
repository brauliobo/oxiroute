# OxiRoute 6.5.0 (Unreleased)

## HTTP/2 and HTTP/3

- The HTTP/2 lane serves services with an access log; the line is the classic path's, with `protocol` `h2`.
- The HTTP/2 lane serves requests whose body has no `content-length` (the origin gets it chunked, as the classic
  path sends it) and requests with an `Expect` field (forwarded as a plain field). A body without a length over the
  route's request body limit is refused with `413` as soon as the limit is passed.
- Reverse HTTP/3 listeners run one QUIC endpoint per worker when `listener_workers` is above 1 (`SO_REUSEPORT`
  sockets on the listener's address); one worker behaves as before.
- Reverse HTTP/3 sends an origin answer that has no declared length, or a declared length above 256 KiB, as it
  arrives instead of buffering it: such an answer has no `content-length` unless the origin gave one, is not bound by
  the 64 MiB response limit, and a failure after the head resets the stream. Cache fills and `HEAD` stay buffered.
- An idle HTTP/2 connection holds less memory: the HPACK tables size their buffers to their entries and a connection
  that has closed its streams gives its stream slots back.
