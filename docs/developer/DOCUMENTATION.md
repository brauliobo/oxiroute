# Documentation Maintenance

Documentation is part of the product contract. The source tree contains detailed specifications, but
readers should not have to understand the entire roadmap before they can run one safe local request.

## Content Hierarchy

Use this order when adding or reorganizing content:

1. **README:** product identity, current boundaries, first run, and links only.
2. **Docs hub:** audience and job navigation.
3. **User guide:** one operational task with copyable commands and failure meaning.
4. **Reference:** exact fields, commands, routes, and status values.
5. **Normative spec:** complete schema, invariants, and future requirements.
6. **Website:** visual orientation, progressive disclosure, and links back to versioned source docs.

Do not hide a current limitation in an expandable panel. A reader deciding whether to deploy must see
it before the feature details.

## Status Rules

Use `implemented`, `partial`, `planned`, and `out of scope` consistently. Every status claim should
point to code/tests or a normative boundary. Product specs and roadmaps describe goals; the README,
compatibility matrix, and release notes describe current behavior.

When a capability changes:

- update the compatibility matrix;
- update the relevant user/reference guide;
- add or update failure-path and observable-state tests;
- update release notes when user-visible; and
- search the website for stale wording.

## Examples

Prefer a complete runnable example plus one small concept snippet. Label examples as local, production
starting points, or illustrative. Never use real hostnames, tokens, private keys, recording roots, or
stream keys in documentation or media.

## Website

The static site lives in `website/` and is deployed as-is by `.github/workflows/pages.yml`. It uses:

- role tabs for operators, migrators, and developers;
- capability tabs for traffic, configuration, observability, media, and migration;
- `details` blocks for reference depth;
- code-format tabs for KDL, Lua, and CLI/API examples; and
- explicit status chips and limitation callouts.

Keep the site useful with JavaScript disabled: headings, links, and primary examples must still be
readable. Test keyboard navigation and a narrow viewport when changing its interaction model.

## Remotion Media

The `remotion/` package owns deterministic dashboard recordings. It intentionally uses fixture data so
rendering never needs a daemon, token, private key, or live network. Render from the repository root:

```sh
pnpm --dir remotion install
pnpm --dir remotion render
```

The scripts write `website/assets/admin-overview.gif` and
`website/assets/admin-configuration.gif`. Review the output as product communication: it must match
current UI labels and must not make a planned control look implemented. The source composition and
render command are the reproducible record; the GIF is a presentation artifact.
