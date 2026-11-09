# Stats page foundation

Second Vite entry of `ui/` (`stats.html` -> `src/stats/main.ts`, pure Vapor) for the page listener. Contract:
`contracts/stats-page.openapi.json`; types are generated, never edited (`pnpm generate:stats-page`, `pnpm verify:stats-page`
-> `generated/statsPage.ts`). Components are `<script setup vapor lang="ts">` + pug and read everything from the store.

## Build output (`pnpm build:stats` -> `ui/dist-stats/`, git-ignored)

```
stats.html               reference shell (not embedded: the Rust shell replaces it)
manifest.json            Vite manifest; entry key "stats.html": { file, css: [...] }
assets/stats-<hash>.js   the whole app (single chunk, ES module, no dynamic imports)
assets/stats-<hash>.css  the whole stylesheet
```

Names are content hashes (deterministic for pinned pnpm/node/vite and `--frozen-lockfile`). The shell must load
`<prefix>/assets/<file from manifest>` as `<script type="module" crossorigin>` and `<link rel="stylesheet" crossorigin>`;
nothing in the bundle assumes the page is at `/`. The page asks `<page path without trailing slash>/snapshot.json`.
`pnpm verify:stats-bundle` (part of `build:stats`) checks: pure Vapor (no VDOM), only the files above, manifest matches,
gzip budget JS <= 60 KiB and CSS <= 12 KiB (`scripts/verify-stats-budget.mjs`). Browser smoke:
`pnpm test:browser --project=stats-chromium` (strict CSP via `vite preview --config vite.stats.config.ts`).

## Modules (all pure and tested, except the store)

| Module | API |
| --- | --- |
| `decode.ts` | `decodeSnapshot(json): StatsSnapshot` (counters -> `bigint`, absent stays absent), `DecodeError{path}`, `UnsupportedSchemaError`, `safeNumber(bigint)` |
| `model.ts` | `buildModel(snapshot): StatsModel` (process, generation, listeners, routes, pools[servers], certificates, watchers, warnings, summary), `rowKey(kind, ...parts)`, `modelCounters`, `modelGauges`, `STATUS_LABELS`. Rows: `{ key, name, wire, problem, active, counters, histograms }` |
| `derive.ts` | `deriveModel(model, rates): MeasuredModel` adds `rates` (per s) and `latency` (`p50/p90/p99`, `meanMs`, `count`) to rows; both absent while unknown |
| `rates.ts` | `createRateTracker().update(sample, maxGapMs)`: outcome `first/rates/same/gap/reset`; no rate across restart, generation change or gap |
| `percentiles.ts` | `quantile(hist, bounds, q, {interpolate})` (nearest rank, bucket upper bound, `overflow` flag), `summarizeLatency` |
| `series.ts` | `Ring`, `SeriesBook` (shared timeline, unknown points are `undefined`) |
| `format.ts` | `formatBytes/Count/Exact/Rate/ByteRate/Duration/Age/Timestamp/Percent`, `truncate`, `UNKNOWN`; no Intl locale dependence |
| `urlState.ts` | `ViewState`, `parseViewState(search)`, `formatViewState(view)`, `replaceUrlState`; keys `q state problems sort paused interval theme open closed` |
| `query.ts` | `queryPools(pools, view)`, `queryListeners(listeners, view)`: text, status chips, problems only, `SORT_COLUMNS` |
| `poll.ts` | `createPoller`, `fetchSnapshot`, `snapshotUrl`; backoff 1x, 2x, ... 30 s on 503 `stats_unavailable`, network, HTTP and decode errors; schema mismatch stops |
| `fixtures.ts` | `wireDocument(overrides)`, `wirePool`, `wireServer`, `wireListener`, `histogram`, `counter`: invented data in contract shape |

## `StatsStore` (`store.ts`; `useStatsStore.ts`)

Root component calls `provideStatsStore()` (creates, starts, provides, disposes with the component); any descendant calls
`useStatsStore()`. Refs are Vue `shallowRef`/`computed`; destructure them at the top of `<script setup>` to use them in
templates unwrapped (`const { model, status } = useStatsStore()`).

- Data: `snapshot` (decoded, kept while stale), `model` (`MeasuredModel`), `visible` (`{ listeners, pools }` narrowed and
  sorted by `view`), `series(key)` / `seriesTimes()` (key `<row key>#<requests|errors|bytesReceived|bytesSent|accepted|active>`).
- Status: `status` = `loading | live | paused | stale | retrying | error | unsupported`, `failure` (kind, attempts,
  `retryInMs`), `ageMs` / `lastUpdatedAtMs` / `nowMs` (1 s tick), `intervalMs`, `rateOutcome` (`reset` = no rates for one
  interval after a reload or restart).
- View (URL-backed, replaces the history entry): `view`, `setView(patch)`, `toggleExpanded(key)`, `toggleCollapsed(key)`,
  `setPaused`, `togglePause`, `setIntervalMs`, `refresh()` (also while paused).

Not done here, for Phase 4: applying `view.theme` to the document, rendering, admin POST (`AdminForm`, needs
`model.admin` and `generation.activeRevision`), `/` shortcut, stylesheet beyond the shell tokens.
