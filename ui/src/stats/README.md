# Stats page foundation

Second Vite entry of `ui/` (`stats.html` -> `src/stats/main.ts`, pure Vapor) for the page listener. Contract:
`contracts/stats-page.openapi.json`; types are generated, never edited (`pnpm generate:stats-page`, `pnpm verify:stats-page`
-> `generated/statsPage.ts`). Components are `<script setup vapor lang="ts">` + pug and read everything from the store.

## Build output (`pnpm build:stats` -> `ui/dist-stats/`, git-ignored)

```
stats.html               the shell, built from ui/stats.html; installed as the embedded index.html
manifest.json            Vite manifest; entry key "stats.html": { file, css: [...] }
assets/stats-<hash>.js   the whole app (single chunk, ES module, no dynamic imports)
assets/stats-<hash>.css  the whole stylesheet
```

Names are content hashes (deterministic for pinned pnpm/node/vite and `--frozen-lockfile`). `ui/stats.html` is the
single source of the shell the page listener serves. The build base is the token `/__stats_prefix__/`, so the built
`stats.html` loads `/__stats_prefix__/assets/<file from manifest>` as `<script type="module" crossorigin>` and
`<link rel="stylesheet" crossorigin>`; the listener replaces the token with the configured `uri_prefix` per request.
`scripts/generate-stats-assets.sh --from ui/dist-stats` installs the built `stats.html` as
`crates/oxiroute-server/assets/stats/index.html` and `verify-stats-assets.sh` fails when it differs from a fresh build.
Nothing in the bundle assumes the page is at `/`; `vite preview` answers the token as the site root. The page asks `<page path without trailing slash>/snapshot.json`.
`pnpm verify:stats-bundle` (part of `build:stats`) checks: pure Vapor (no VDOM), only the files above, manifest matches,
gzip budget JS <= 90 KiB and CSS <= 12 KiB (`scripts/verify-stats-budget.mjs`). Browser smoke:
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

## Page chrome (`StatsShell.vue`, `components/`)

`StatsShell` provides the store and composes skip link, header (title, `StatsStatus`, CSV and JSON links), `StatsToolbar`,
`StatusBanner`, `AdminActions` (only when `model.admin` and `generation.activeRevision`), `<main id="ox-main">` with
`StatsDashboard` (the data views), `Footer` and `ShortcutsDialog`. It applies `view.theme` to `<html data-theme>`
(`theme.ts`; `auto` removes it) and installs the single-key shortcuts of `shortcuts.ts` (`/ ? p r t f`, never while typing,
behind a dialog or with Ctrl/Alt/Meta). Leaf controls are pure props/emits (`update:modelValue` style); `StatsToolbar`
wires them to the store. `banner.ts` turns `status` + `failure` into the banner text (no ticking values: it is an alert
region). `admin.ts` posts the urlencoded form to the page path. `stats.css` holds the `--ox-*` tokens (`light-dark()`,
system scheme unless `data-theme` forces one), focus ring, forced-colors, reduced-motion, narrow-screen and print rules.

The shell (`ui/stats.html`, built into the embedded `crates/oxiroute-server/assets/stats/index.html`) keeps the refresh
meta and the server-rendered table (`__stats_table__`) inside `<noscript>`, so a browser with script shows neither.
