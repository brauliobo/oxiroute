# OxiRoute 0.5.5 (draft)

OxiRoute 0.5.5 lets a pool keep serving when health checks or passive ejection would otherwise take
it to zero servers. The release remains pre-alpha and retains conservative compatibility claims.

## Highlights

- A new per-pool `resilience` object groups availability options; every option defaults to the
  previous behavior, so existing configurations are unchanged.
- `resilience.fail_open = true`: when no server of the pool is selectable because of health checks
  or passive ejection, the pool selects the least bad server instead of answering an empty `503`.
  Administrative drain, maintenance and a forced-down health override are still honoured, and
  `max_connections` and the queue still apply. A single-backend pool whose application also fails
  its own health path now returns the application's error response.
- `resilience.min_members` (default 1 with `fail_open`, otherwise 0) keeps that many servers
  selectable: passive ejection no longer takes the last selectable server out of rotation, so a
  short burst of application errors cannot turn into a whole backoff of `503` answers.
- `resilience.ejection_jitter_percent` (0 through 50) randomizes each passive ejection duration so
  servers ejected together do not return together; the maximum backoff still bounds it.
- `oxiroute_pool_fail_open_selections_total` and the pool `failOpenSelections` field of the
  management API count the selections made this way.

## Operational Notes

Fail-open applies to HTTP and L4 pools alike. Alerting on `oxiroute_pool_available_servers == 0`
keeps working: the strict count does not include failed-open servers.

Rollback: install the previous package and restart the service. A configuration that uses
`resilience` must drop the object first, because the previous release rejects unknown fields.
