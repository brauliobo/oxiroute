# OxiRoute website

This is the static public documentation site for OxiRoute. It intentionally has no framework or
runtime dependency so GitHub Pages can upload the directory directly.

## Preview Locally

From the repository root:

```sh
python3 -m http.server 8000 --directory website
```

Open `http://127.0.0.1:8000/`. The site expects the rendered GIFs at `assets/admin-overview.gif` and
`assets/admin-configuration.gif`; render them from `remotion/` when they are missing or when the UI
states change.

## Content Rules

- Keep the pre-alpha status and capability boundaries visible before feature detail.
- Use tabs for audience or capability lenses, not for hiding limitations.
- Keep `<details>` content useful when JavaScript is disabled.
- Link exact command/API/configuration contracts to versioned repository docs.
- Do not include tokens, private keys, recording roots, stream query arguments, or live production data.

The Pages deployment workflow is `.github/workflows/pages.yml`.
