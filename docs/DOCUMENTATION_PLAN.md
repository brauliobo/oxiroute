# Documentation Plan

This plan records the communication model derived from the current workspace, management API, Vue/Pug
dashboard, importer boundaries, RTMP runtime, supervision foundations, and release packaging.

## Product Findings

OxiRoute has three different kinds of truth that need different presentation:

1. **Operational truth:** active listeners, pools, health, generations, RTMP sessions, counters, and
   readiness. Operators need commands, evidence, and recovery actions.
2. **Configuration truth:** typed objects, source formats, revisions, previews, composition, and
   validation. Integrators need examples and exact contracts without reading Rust types.
3. **Implementation truth:** crate ownership, runtime planning, state machines, test layers, and
   unsupported behavior. Developers need architecture and failure-path context.

The old README mixed all three into one inventory. The new hierarchy separates them while preserving
links to the normative specs.

## Audience Lanes

| Lane | Primary question | Entry point | Success signal |
| --- | --- | --- | --- |
| Operator | Can I run and safely change this instance? | `docs/user/GETTING_STARTED.md` | A verified local request and a readable runtime state |
| Existing proxy operator | What will migrate, and what will not? | `docs/user/MIGRATION.md` | Report reviewed, preview finalized, native source preserved |
| Media operator | Can I publish, observe, and record this stream? | `docs/user/RTMP.md` | Live catalog and recorder phase match the configured policy |
| API integrator | What endpoint, auth, revision, or number type should I expect? | `docs/reference/API.md` | Client handles success, stale state, and redacted failure accurately |
| Rust/UI developer | Where does this behavior belong, and how is it proved? | `docs/developer/ARCHITECTURE.md` | Owning abstraction, focused test, compatibility update |
| Release maintainer | Can this claim and artifact be shipped reproducibly? | `docs/developer/RELEASING.md` | Gates, docs, media, package, and source archive agree |

## Information Architecture

```text
README.md
  -> docs/README.md
      -> docs/user/*
      -> docs/reference/*
      -> docs/developer/*
      -> existing normative specifications
  -> website/index.html
      -> role tabs
      -> capability tabs
      -> dashboard recordings
      -> versioned GitHub source docs
```

The README answers identity, current status, first run, boundaries, and where to go next. User pages
answer one task at a time. Reference pages answer exact shape questions. Normative specs hold the
complete contract. Developer pages explain ownership and contribution. The website compresses the
same hierarchy into a navigable visual overview.

## Interaction System

The website uses containers according to information density:

- **Role tabs:** operator, migrator, developer; each exposes a different first action.
- **Capability tabs:** traffic, control plane, RTMP media, migration, supervision; each separates the
  useful current slice from its boundary.
- **Expandable details:** generation rationale, counter encoding, import blocking, exclusions, and
  other depth that should not interrupt the first read.
- **Code cards:** short commands near the concept, with the full runnable example in the repository.
- **Status chips and boundary strips:** implemented/partial/planned state stays visible rather than
  hidden in a reference table.
- **GIF figures:** visual proof of the dashboard's vocabulary, paired with text descriptions and no
  live data.

All interactions have a readable HTML fallback. The site uses no runtime framework, remote font, API
call, cookie, or tracker. The primary content is available without JavaScript; JavaScript adds tab
state, keyboard tab movement, and copy convenience.

## Source Of Truth

| Information | Authoritative file |
| --- | --- |
| Current feature status | `README.md`, `docs/COMPATIBILITY.md`, release notes |
| Complete configuration behavior | `docs/CONFIG_SPEC.md`, `docs/CONFIG_FORMATS.md` |
| Management/API behavior | `docs/API_UI_SPEC.md`, `docs/MANAGEMENT_CLI.md` |
| Operations and lifecycle | `docs/OPERATIONS.md` |
| Native import boundary | `docs/IMPORT_SPEC.md` |
| RTMP behavior | `docs/RTMP_SPEC.md` |
| Test/release gates | `docs/TEST_STRATEGY.md`, `docs/developer/RELEASING.md` |
| Public orientation | `website/index.html`, linked back to the files above |

When code changes a public behavior, update the authoritative contract first, then the relevant user
page and website status. Never make the website more optimistic than the compatibility matrix.

## Dashboard Media Plan

| Asset | Composition | Message |
| --- | --- | --- |
| `website/assets/admin-overview.gif` | `AdminOverview` | Monitoring, listeners, pool health, and RTMP inventory are one live readout |
| `website/assets/admin-configuration.gif` | `AdminConfiguration` | Disk/active revisions, typed editing, validation, and review precede save |

The source is in `remotion/src/DashboardRecording.tsx`. It uses fixture data and shipped labels, then
renders through `remotion/package.json` into GIFs. The workflow is intentionally deterministic and
credential-free. A changed UI label, status, or boundary requires a media review before publishing.

## Pages Deployment

1. A push to `main` or a manual dispatch runs `.github/workflows/pages.yml`.
2. The workflow verifies the static entrypoint and both GIFs.
3. `website/` is uploaded with `actions/upload-pages-artifact`.
4. `actions/deploy-pages` publishes the artifact at the repository Pages URL.

Repository settings must select GitHub Actions as the Pages source. The workflow does not start the
daemon or expose the management API; it publishes documentation only.

## Maintenance Checklist

- Can a new operator find a verified local run in two clicks?
- Is every visible capability labeled implemented, partial, planned, or out of scope?
- Does each example use the current CLI/config/API shape?
- Are secrets, roots, stream queries, and production data absent from prose and media?
- Do tabs and expanders preserve a useful keyboard and no-JavaScript path?
- Do dashboard GIFs still match shipped labels and controls?
- Does the Pages artifact contain the entrypoint, styles, script, and media?
- Have focused tests and the full gates been run for any changed runtime claim?
