# OxiRoute 6.3.0: the stats page (release notes entry, draft)

The built-in page of a `stats.pages[]` listener is now an interactive dashboard: listeners, routes, pools
and servers with details, generation, process, certificates and warnings, live rates, latency percentiles,
filters, sorting, pause, theme, keyboard shortcuts, CSV and JSON export, and, for loopback peers of a page
with `admin = "localhost"`, Ready, Drain and Maintenance with a confirmation. It is read-only, needs no
token, and ships inside the binary. This entry is meant to be merged into the 6.3.0 release notes.

## Summary

- **Interactive page.** `GET <uri_prefix>` serves a script page that polls `snapshot.json`. A browser with
  script disabled gets the table of pools and servers inside `noscript`, as before.
- **New endpoints on the page listener.** `<uri_prefix>/snapshot.json` (the whole snapshot, contract
  `contracts/stats-page.openapi.json`), `<uri_prefix>/stats.csv` (HAProxy-style columns) and
  `<uri_prefix>/assets/<name>` (the embedded, content-hashed page files).
- **One snapshot per second.** The snapshot is built once per second and shared by every viewer, so a page
  costs the same with one viewer or many. It adds no counter to the request path.
- **No node for users or packagers.** The built page is committed and compiled into the binary, so
  `cargo build` and the package build (AUR unchanged) need no node. `pnpm build:stats` and a CI drift check
  are for contributors.
- Design and contract: [OBSERVABILITY.md](design/OBSERVABILITY.md), [API_UI_SPEC.md](../API_UI_SPEC.md),
  [OPERATIONS.md](../OPERATIONS.md), [DASHBOARD.md](../user/DASHBOARD.md).

## Measured effect

Measured on a release build without LTO, loopback, one page listener, 1008 servers in four pools of 250 (all
reachable, health checks every 2 s) and `curl` as the client; one run, not a benchmark lane.

- `snapshot.json` is 1.13 MB (18.9 KB with gzip, which the listener does not apply: the bytes on the wire are the
  raw size). A request that rebuilds the snapshot takes 5 to 7 ms end to end; a request answered from the cache
  within the same second takes about 0.5 ms. `stats.csv` is 68 KB.
- Ten viewers polling every 2 s for 30 s cost the process 0.8 s of CPU in total (2.7 % of one core, health checks
  included) and 53 MiB resident. Viewers share one snapshot build per second.
- A viewer of the 1008-server configuration downloads about 560 KB/s while the page is open and visible; the page
  at its default interval is the dominant cost, not the build.
- The no-script table inside the shell grows with the server count: the shell is 498 KB for 1008 servers
  (7.8 KB with gzip) and about 0.8 KB for none.
- Page weight: shell 783 B, script 148 KB, stylesheet 16 KB; with gzip 378 B, 50.2 KB and 4.1 KB (54.7 KB in all).
  The files are immutable and cached, so a repeat visit costs only the snapshots.

## Behaviour changes to know before upgrading

- **The page at `uri_prefix` is a script page.** Its table now sits inside `noscript`. Scripts that scraped
  the HTML table with a plain HTTP client keep getting that table in the response body, but should move to
  `snapshot.json` or `stats.csv`.
- **Sub-paths other than `snapshot.json`, `stats.csv` and `assets/` answer 404.** Before, any path that
  started with `uri_prefix` returned the table.
- **The prefix match is exact.** `/stats` and `/stats/` are the page; a sibling such as `/statsfoo` is no
  longer served by a page with prefix `/stats`.
- **Unchanged.** The legacy token-gated `/stats` and `/api/v1/status` and the public `/metrics`, `/ready`
  and `/status.json` on `stats.binds` behave as before, and a page listener still does not serve them.
- **Strict Content-Security-Policy.** Page responses send `default-src 'none'; script-src 'self';
  style-src 'self'; connect-src 'self'; img-src 'self' data:; font-src 'self'; form-action 'self';
  base-uri 'none'; frame-ancestors 'none'`. There is no inline script or style and no external request.
  A fronting proxy must not rewrite the header or inject scripts, or the page will not run.
- **The page shows everything to anyone who reaches the page listener.** That includes upstream addresses,
  listener binds, generation revisions, certificate names and expiry, and configuration warnings. There is no
  reduced public view. Keep a page on loopback or a trusted interface and put access control in front of
  anything wider.
- **`refresh_ms` is a polling hint.** It is reported as `intervalHintMs` and the page never polls faster than
  1 s; values below 1000 act as 1000.
- **Snapshots are cached for 1 s.** `snapshot.json` and `stats.csv` can be up to a second old; a change made
  through the page administration is visible at once.

## Upgrade notes

- No configuration change is needed. Existing `stats.pages[]` entries get the new page after the restart.
- Contributors: build the page with `pnpm build:stats` in `ui/`, install it with
  `scripts/generate-stats-assets.sh --from ui/dist-stats`, and check it with
  `scripts/verify-stats-assets.sh`; CI runs the check in the `stats-ui` job.
- `vue` and `@vitejs/plugin-vue` are pinned to exact versions because Vue 3.6 (Vapor) is a release candidate.

## Known limits

- Not in this version: peak gauges (`qmax`, `smax`), connect time, per-route request counters, per-listener
  response status classes, circuit-breaker state, and health-check duration and history. They need new
  recording on the request path and are absent from the snapshot, never shown as zero.
- Latency percentiles come from lifetime histograms and are bucket upper bounds.
- Rates and sparklines start empty when the page loads; the server keeps no history.
