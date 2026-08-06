# OxiRoute Documentation

This documentation is organized by the decision a reader is trying to make. Start with a job guide,
then open the reference contract only when you need exact fields, bounds, or failure behavior.

The full communication model, source-of-truth rules, site interaction design, and media/deployment
workflow are recorded in the [documentation plan](DOCUMENTATION_PLAN.md).

## Choose A Path

| Audience | First stop | Then |
| --- | --- | --- |
| New operator | [Getting started](user/GETTING_STARTED.md) | [Dashboard](user/DASHBOARD.md), [operations](user/OPERATING.md) |
| Existing proxy operator | [Migration](user/MIGRATION.md) | [compatibility matrix](COMPATIBILITY.md), [import specification](IMPORT_SPEC.md) |
| RTMP operator | [RTMP guide](user/RTMP.md) | [RTMP specification](RTMP_SPEC.md), [dashboard](user/DASHBOARD.md) |
| API/UI integrator | [API reference](reference/API.md) | [API/UI specification](API_UI_SPEC.md), [management CLI](MANAGEMENT_CLI.md) |
| Rust contributor | [Developer guide](developer/README.md) | [architecture](developer/ARCHITECTURE.md), [testing](developer/TESTING.md) |
| Release or package maintainer | [Release guide](developer/RELEASING.md) | [Arch packaging notes](../packaging/arch/README.md) |

## User Guides

- [Getting started](user/GETTING_STARTED.md): install/build, run the example, and verify traffic.
- [Dashboard](user/DASHBOARD.md): understand Overview, Statistics, topology, RTMP, and Configuration.
- [Operations](user/OPERATING.md): health, generations, drains, server state, metrics, and recovery.
- [Migration](user/MIGRATION.md): report versus preview, native references, composition, and cutover.
- [RTMP](user/RTMP.md): live publish/play, fanout, recording, HLS/DASH, VOD, and current codec limits.
- [Troubleshooting](user/TROUBLESHOOTING.md): symptoms, evidence, and safe next actions.
- [Security](user/SECURITY.md): loopback boundaries, token files, source trust, and redaction.

## Developer Guides

- [Developer guide](developer/README.md): contribution route, crate map, and behavior boundaries.
- [Architecture](developer/ARCHITECTURE.md): source-to-generation flow, data plane, control plane,
  RTMP, and supervision.
- [Testing](developer/TESTING.md): test layers, focused commands, and release gates.
- [Documentation maintenance](developer/DOCUMENTATION.md): status labels, examples, website content,
  and Remotion media.
- [Releasing](developer/RELEASING.md): versioning, packages, source archives, and Pages deployment.

## Reference Contracts

- [CLI reference](reference/CLI.md): command families, output modes, authentication, and exit codes.
- [API reference](reference/API.md): endpoint groups, authentication, revisions, and response rules.
- [Configuration reference](reference/CONFIGURATION.md): formats, smallest useful objects, and save behavior.
- [Configuration specification](CONFIG_SPEC.md): normative schema and validation contract.
- [Configuration formats](CONFIG_FORMATS.md): KDL, Lua, HOCON, UCI, templates, and native references.
- [Management CLI](MANAGEMENT_CLI.md): complete capability matrix and HAProxy intent mapping.
- [API/UI specification](API_UI_SPEC.md): exact control-plane response and browser behavior.
- [Compatibility matrix](COMPATIBILITY.md): implemented, partial, planned, and excluded features.
- [Operations contract](OPERATIONS.md): generations, watcher behavior, shutdown, readiness, and metrics.
- [Import specification](IMPORT_SPEC.md): native source boundaries and provenance requirements.
- [RTMP specification](RTMP_SPEC.md): directive inventory, runtime behavior, and compatibility status.
- [ACME specification](ACME_SPEC.md): managed lifecycle, challenge types, Certbot lineage behavior, and release gates.
- [Test strategy](TEST_STRATEGY.md): repository-wide test policy and release gates.
- [Roadmap](ROADMAP.md): milestones and future work, not a support promise.

## Status Vocabulary

Use the same words everywhere:

- `implemented`: code and initial tests are present.
- `partial`: a narrow path works, but a broader compatibility or production gate remains.
- `planned`: a goal or roadmap item; not available in the current daemon.
- `out of scope`: intentionally outside the user-space proxy boundary.

The README and compatibility matrix describe current behavior. Product specs and roadmaps describe
requirements and direction; they must not be used alone to decide whether a feature is deployable.
