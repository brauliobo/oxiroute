# UI design system

The console (`ui/index.html`) and the public stats page (`ui/stats.html`) share one design system in `ui/src/design`.
Pages are not shared: `design/` imports nothing from the console, the stats page or the API, and the stats page never
imports the console. `src/sfcPolicy.test.ts` checks the boundaries.

## Layout

```
ui/src/design/
  index.css            layer order and the imports of tokens/ and base/
  tokens/              color, space, type, shape, motion, density (--ox-* custom properties)
  base/                reset, document, focus ring, forced colours, more contrast, reduced motion, print, no-script table
  <primitive>/         Pug + <script setup vapor> components, types.ts, one styles.css, one test
  composables/         useTheme, useDensity, useShortcuts, useLiveRegion
  directives/cssVars.ts  CSS custom properties from a template, through the CSSOM (strict CSP, no inline styles)
  format.ts            locale-independent number, size, duration and time formatting
```

Import with the `#design` alias (`import Button from '#design/button/Button.vue'`, `import '#design/index.css'`), defined
once in `ui/vite.shared.ts` and `tsconfig.json`. A page imports `#design/index.css` first, then its own CSS.

Primitives: Button, Chip, Toggle (`button/`), Badge, StatusDot (`badge/`), Field, Input, Select, Checkbox (`field/`),
Table, SortHeader (`table/`), Panel, Card (`panel/`), Banner, Notice (`banner/`), EmptyState, Loading (`empty/`),
Dialog, Actions (`dialog/`, native `<dialog>`), Toast, Region (`toast/`), Tabs, Nav, Sparkline, Meter, Kbd, and the page
Shell with ThemeToggle (`layout/`). `ui/gallery.html` (dev only, `pnpm dev`, not in either build) renders all of them.

## Tokens

- Colours are `light-dark(light, dark)` pairs of hex values in `tokens/color.css`, the only place a colour literal is
  allowed. `data-theme="light|dark"` on `<html>` forces a scheme, otherwise the system decides (`useTheme`).
- Neutrals are the stats page palette. The brand is a lime accent: `#b6ff51` on dark, the darker `#456f0c` on light so it
  passes contrast. `--ox-on-brand` is the text on a brand fill. Status colours (`ok`, `warn`, `bad`, `info`) are tuned so
  text passes 4.5:1 on the page, on cards and on the 10% tint of a badge.
- `tokens/tokens.test.ts` parses every pair with a CSS parser and checks the contrast of every text, badge, brand and
  control pair in both themes. Change a colour only with that test green.
- Scales: `--ox-space-1..9`, `--ox-text-2xs..2xl`, `--ox-radius-*`, `--ox-z-*`, `--ox-weight-*`, `--ox-duration*`.
- Density: `data-density="compact"` (stats page, default) or `"comfortable"` (console) changes `--ox-gap`, `--ox-pad`,
  `--ox-control-h`, `--ox-control-px` and the table cell padding. Primitives size from those tokens; they never hard-code a
  height.
- System font stacks only: no web fonts (the stats page has a byte budget and a strict CSP).
- Breakpoints are `45rem`, `64rem` and `80rem`.

## CSS layers

`@layer ox.reset, ox.tokens, ox.base, ox.primitives, ox.page`. Tokens and base styles live in their layers; every
primitive `styles.css` is wrapped in `ox.primitives`; page CSS is wrapped in `ox.page`, so a page always wins over a
primitive without specificity tricks. Global classes start with `ox-`. Primitives import their own `styles.css` so a page
only ships the primitives it uses.

## Pug style

- Tag shorthand: `Button.ox-more(small :aria-expanded="open")`, `Select#ox-sort(...)`, attributes in parentheses.
- Inline whitespace with `|` pipes or `#[Tag text]`; no mixins, no includes.
- No TypeScript in template expressions (`as`, generics): put a function in `<script setup>`.
- Booleans on `aria-*` attributes are real booleans (`:aria-busy="loading"`), never `'' : null`. `vue-tsc` type-checks Pug
  through `@vue/language-plugin-pug`.
- Pages use primitives, not raw `button`, `input`, `select`, `textarea`, `table` or `dialog`. No `h()`, `defineComponent`,
  render functions, TSX or JSX, no `<style>` in design, stats or gallery SFCs, no inline styles (use `v-css-vars`).
- Stays pure Vapor: no interop, no VDOM component libraries, exact `vue` pin.

## Adding a primitive

1. Create `ui/src/design/<name>/` with `<Name>.vue` (`<script setup vapor lang="ts">` plus a Pug template), `styles.css`
   (everything inside `@layer ox.primitives`, tokens only), `types.ts` when types are shared, and `<name>.test.ts` using
   `src/test/mount.ts` (`props`, `slots`, `emitted()`).
2. Import `./styles.css` from the component. Build accessibility in: a native element first, labels, `aria-*` states as
   booleans, a visible focus ring from the base styles, a forced-colors rule where colour carries state, state as word or
   shape and never colour alone.
3. Let native attributes and listeners fall through to the root element. Boolean props that must be absent rather than
   `false` default to `undefined`.
4. Add it to the gallery (`ui/src/gallery/sections`) so the axe checks cover it in every theme and density.

## Enforcement

| Check | What it holds |
| --- | --- |
| `pnpm lint:css` (stylelint, `ui/stylelint.config.mjs`) | design, stats and gallery: no hex or colour functions, no `url()` or `@import`, no `!important`, tokens for colour, spacing, font, radius and z-index, `ox-` classes, breakpoint allowlist. Exceptions: tokens may hold colours, `index.css` imports, `base/` may use `!important` |
| `scripts/verify/ui/colors.sh` | console: hard-coded colours per file, shrink-only baseline in `scripts/verify/ui/baselines/colors.txt` |
| `scripts/verify/ui/duplicates.sh` | console: classes styled in several files, shrink-only baseline in `baselines/duplicates.txt` |
| `src/sfcPolicy.test.ts` | Vapor, script setup, Pug; no raw elements outside design (console baseline in `src/sfcPolicy.baseline.json`, shrink-only); import boundaries |
| `pnpm verify:css` | the three above, run by the `ui` CI job |

Lint messages say what to use instead. Fix the cause (use a token or a primitive, or add the missing one); do not raise a
baseline. Delete a baseline entry as soon as its file is clean.

## Stats page budget

`pnpm build:stats` fails above JS 90 KiB gzip or CSS 12 KiB gzip, on any extra chunk or asset, or when a VDOM renderer
appears. Keep primitives small, import only what the page uses, no web fonts and no `url()`; icons are inline SVG paths.
Any change reachable from the stats page changes the embedded assets: run `scripts/generate/stats/assets.sh --from
ui/dist-stats` (Node 22 and pnpm 11.3.0, `pnpm install --frozen-lockfile`) and commit `crates/oxiroute-server/assets/stats`
and the generated table; `scripts/verify/stats/assets.sh` fails CI when they differ from a fresh build.

## Tests

- `pnpm test`: primitive unit tests, token contrast, base styles, SFC policy.
- Playwright `gallery-chromium`: axe checks of the gallery in each theme and density and of the stats page in both
  themes, plus behaviour checks of the dialog, tabs and the theme and density switches. Pixel snapshots are not used:
  fonts differ between machines, so they cannot be reproduced.
