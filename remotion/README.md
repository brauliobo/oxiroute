# OxiRoute dashboard recordings

This is a deterministic Remotion source for the dashboard GIFs used by the README, user docs, and
GitHub Pages site. It renders representative fixture data rather than connecting to a daemon. No
management token, private key, recording root, or live network is needed.

## Render

```sh
pnpm install
pnpm render
```

The scripts write:

- `../website/assets/admin-overview.gif`
- `../website/assets/admin-configuration.gif`

Preview a composition before rendering:

```sh
pnpm studio
```

The compositions intentionally use labels and states from the shipped Vue/Pug dashboard. If the UI
changes, update this source and review the output as documentation rather than treating it as a
generic marketing animation.
