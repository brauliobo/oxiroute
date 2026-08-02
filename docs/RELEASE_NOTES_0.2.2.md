# OxiRoute 0.2.2

OxiRoute 0.2.2 makes bounded upstream admission fair for nonreusable HTTP and L4 traffic.

## Highlights

- Admit queued L4 and `connection_reuse = "never"` HTTP requests through one pool-level FIFO.
- Assign the oldest waiter to whichever eligible server releases capacity next instead of pinning
  pending work to one saturated endpoint.
- Remove cancelled or timed-out waiters exactly once and immediately advance the next request.
- Keep immediate selection unchanged when `queue_timeout_ms` is absent and preserve reusable HTTP's
  connection- and stream-reuse-first connector path.
- Expose selector-queue occupancy, cumulative admission, timeout, and cancellation metrics for the
  FIFO path without counting connector-internal reusable HTTP waits.

## Compatibility

- No configuration migration is required.
- FIFO order begins when a request enters upstream scheduler admission after routing and access
  checks; it does not reorder work already executing on an upstream worker.
- Reusable HTTP requests are intentionally outside the FIFO guarantee because an idle connection or
  reusable HTTP/2 stream can satisfy them without acquiring a new physical connection lease.
