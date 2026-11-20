# Documentation

Choose a task below. Guides explain how to complete it; references define exact commands, fields,
and failure behavior. Use the
[compatibility matrix](COMPATIBILITY.md) to decide whether a required capability is available.

## Start here

| You want to | Start with | Then |
| --- | --- | --- |
| Evaluate a local proxy | [Getting started](user/GETTING_STARTED.md) | [Dashboard](user/DASHBOARD.md), [operations](user/OPERATING.md) |
| Install a release package | [Install](user/INSTALL.md) | [Getting started](user/GETTING_STARTED.md), [security](user/SECURITY.md) |
| Operate a deployment | [User guides](user/README.md) | [Runbook](user/RUNBOOK.md), [security](user/SECURITY.md), [troubleshooting](user/TROUBLESHOOTING.md) |
| Migrate another proxy | [Migration](user/MIGRATION.md) | [Compatibility](COMPATIBILITY.md), [import contract](IMPORT_SPEC.md) |
| Publish or record media | [RTMP guide](user/RTMP.md) | [RTMP contract](RTMP_SPEC.md) |
| Integrate the management API | [API reference](reference/API.md) | [API/UI contract](API_UI_SPEC.md), [CLI](reference/CLI.md) |
| Contribute code | [Developer guide](developer/README.md) | [Architecture](developer/ARCHITECTURE.md), [testing](developer/TESTING.md) |
| Evaluate speed or resource use | [Benchmarks](BENCHMARKS.md) | [7.1 report](../benchmarks/reports/7.1/README.md): workloads, run counts, and reproduction limits |
| Package or release | [Releasing](developer/RELEASING.md) | [Arch packaging](../packaging/arch/README.md) |

## Guides and explanations

- [User guides](user/README.md): running, operating, migrating, securing, and diagnosing the daemon.
- [Developer guide](developer/README.md): setup, ownership, and contribution workflow.
- [Architecture](developer/ARCHITECTURE.md): configuration resolution, generations, and runtime boundaries.
- [Testing](developer/TESTING.md): focused commands, required gates, and what each layer proves.
- [UI design system](developer/UI_DESIGN_SYSTEM.md): tokens, primitives, density, Pug style, and the stats page budget.
- [Environment variables](developer/ENV_TO_CONFIG.md): runtime inputs and their configuration equivalents.
- [Benchmarks](BENCHMARKS.md): what was measured and what the results establish; the [7.1 report](../benchmarks/reports/7.1/README.md) holds the full tables.
- [Documentation maintenance](developer/DOCUMENTATION.md): language, examples, and evidence policy.
- [Documentation research and design](developer/DOCUMENTATION_DESIGN.md): rationale and external sources for this structure.

## Exact references and contracts

| Need | Reference | Complete contract |
| --- | --- | --- |
| Commands and exits | [CLI](reference/CLI.md) | [Management CLI](MANAGEMENT_CLI.md) |
| API endpoints and revisions | [API](reference/API.md) | [API/UI specification](API_UI_SPEC.md), [OpenAPI](../contracts/control-plane.openapi.json) |
| Configuration and formats | [Configuration](reference/CONFIGURATION.md) | [Schema](CONFIG_SPEC.md), [formats](CONFIG_FORMATS.md) |
| Current support | [Compatibility matrix](COMPATIBILITY.md) | [Current release notes](RELEASE_NOTES_7.1.1.md) |
| Runtime lifecycle | [Operations guide](user/OPERATING.md) | [Operations contract](OPERATIONS.md) |
| Native import | [Migration guide](user/MIGRATION.md) | [Import specification](IMPORT_SPEC.md) |
| Media | [RTMP guide](user/RTMP.md) | [RTMP specification](RTMP_SPEC.md) |
| Managed certificates | [Security guide](user/SECURITY.md) | [ACME specification](ACME_SPEC.md) |

The [reference index](reference/README.md) collects the command, configuration, and API entry points.

## Read status precisely

| Status | Meaning |
| --- | --- |
| `stable` | Implemented and evidenced within the stated narrow release contract |
| `partial` | An implemented path has remaining compatibility or evidence gates |
| `foundation` | Tested component code, without an active daemon capability |
| `planned` | Future goal, unavailable in the current daemon |
| `research` | Requires a product or design decision |
| `not-planned` | Deliberately excluded from the current plan |
| `out-of-scope` | Outside the user-space proxy boundary |

`stable` describes the named behavior, not overall production maturity or complete parity with another
product. Roadmaps and product specifications contain future requirements; use the compatibility
matrix and the current runtime evidence when making deployment decisions. Performance
tables describe their recorded builds, host and workloads, not a guarantee for another deployment.
