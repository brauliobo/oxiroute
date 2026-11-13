# OxiRoute 6.5.0

Unreleased.

## Fixes

- A request whose client closes its connection while the request waits for an origin is dropped at once. The wait
  ends and its place in the pool queue and its slot are freed (the retry delay, the queue for a free slot, the
  connection to the origin), and the origin never receives the request. Before, an abandoned request stayed queued,
  was sent to the origin when a slot freed up and ran there for nobody. This holds for HTTP/1.1 (fast lane and
  classic path, including a request whose body was announced but not sent), HTTP/2 (stream reset and connection
  close) and HTTP/3. `oxiroute_pool_queue_cancellations_total{pool}` counts the requests that left the queue this
  way, and the debug target `oxiroute::queue` names each one (request id, route and pool). A request that was sent
  to the origin already is handled as before. An HTTP/1.1 client that half-closes its connection (`shutdown` of the
  write side) while its request is queued is treated as gone, as the classic path always treated it once the
  request was sent.
