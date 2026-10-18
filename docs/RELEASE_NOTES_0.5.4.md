# OxiRoute 0.5.4

OxiRoute 0.5.4 stops a cluster pool from sending all of its traffic to a node that cannot serve it.
The release remains pre-alpha and retains conservative compatibility claims.

## Highlights

- When none of the servers of a pool can take a request (all drained, down or in maintenance), the
  503 that OxiRoute answers now carries an `x-oxiroute-pool-unavailable: 1` header.
- A proxy route that receives such a 503 from its upstream treats that server as unavailable for the
  request and redispatches it to the next server of its pool before any response byte is sent. The
  request had not been forwarded, so this does not depend on the method, the body, the retry
  triggers, `response_statuses` or `target`; it still requires a positive `max_retries`, a
  non-upgrade request, a body that can be replayed from the retry buffer, and a distinct server left
  to try.
- Before this, a node reached by a `first` cluster pool whose own worker pools were all drained still
  answered its health check with 200, stayed `healthy` in the cluster pool and answered every request
  with an immediate 503, while the other nodes never received traffic.
- A 503 without the header, such as one sent by an application, keeps its previous behaviour and is
  retried only when `response_statuses` lists it.

## Operational Notes

Upgrade note: install 0.5.4 on every node that fronts a cluster pool. A node that is not upgraded
still answers 503 without the header, so its callers keep returning that 503. Runtime-only state,
such as servers set to drain with `oxiroute server drain`, is lost when the service restarts, so
re-apply it afterwards.

Every request that reaches a drained node still costs one immediate 503 round trip before it moves
on, because a cluster pool without `passive_health` does not remember the node as unavailable. With
`passive_health` configured, that 503 is recorded as an `unexpected_status` failure under the pool's
passive-health policy.

Rollback: install the previous package and restart the service. The configuration format is
unchanged.

## Verification Boundary

Coverage includes a two-node wire test in which the first node of a `first` cluster pool has every
local server drained and a `POST` with a body is served by the second node, and a test in which every
node is drained and the client receives the 503 with the marker header. The HTTP/3 upstream path does
not redispatch on this marker.
