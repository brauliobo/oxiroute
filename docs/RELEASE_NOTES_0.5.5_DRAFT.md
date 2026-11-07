# OxiRoute 0.5.5 (draft)

OxiRoute 0.5.5 makes upstream timeouts and retries predictable. The release remains pre-alpha and
retains conservative compatibility claims.

## Highlights

- A hung backend or an expired request deadline now answers `504` with an `x-oxiroute-reason`
  header (`connect_timeout`, `upstream_header_timeout`, `upstream_write_timeout`, `total_deadline`)
  instead of dropping the connection. Only the listener's own client timeouts still close silently.
- New per-route `policy.timeouts` with optional `connect_ms`, `header_ms` (time to the first response
  byte), `idle_ms` (between response body reads) and `total_ms`. They take precedence over the pool
  `server_timeout_ms` and the route `read_timeout_ms`, and a per-attempt `header_ms` leaves retry
  budget inside the total deadline.
- `retry.response_statuses` accepts any status between 100 and 599.
- New per-route `policy.rescue`: when a backend answers a listed status (for example a `404` from a
  node that lacks a model), the request is replayed on another server of the pool or on a named
  rescue pool, each server at most once. If every candidate answers a rescue status, the client
  receives the last backend response unchanged. Rescues are counted in
  `oxiroute_http_rescues_total{route,status}` and are not passive health failures.
- New `retry.idempotency_header`: with `method_safety = "all"`, a `POST` (or any method but `GET` and
  `HEAD`) is replayed after the request was sent only when it carries that header, such as
  `Idempotency-Key`. Without the header a response timeout or a retried status no longer runs the
  work twice.
- New `retry.budget` (`per_second`, `ratio_percent`) caps the retries of a route so that they cannot
  amplify an outage, and `retry.jitter_percent` spreads the same-server retry delay.

## Upgrade notes

- Requests that used to end with `502` because an upstream read, write or connect timeout fired now
  end with `504`.
- Rescue and the retry budget are rejected for HTTP/3 upstream pools.
- `delay_ms` still applies only before same-server retries; a next-server retry is immediate.
