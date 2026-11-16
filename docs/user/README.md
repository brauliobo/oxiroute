# User and operator guides

Start with [Run a local proxy](GETTING_STARTED.md). It builds from source, sends a request through
the HTTP listener, and checks the daemon without installing a service.

## Choose your next task

| Goal | Guide | Result |
| --- | --- | --- |
| Run and verify a local instance | [Getting started](GETTING_STARTED.md) | HTTP `200` through the proxy and readable runtime state |
| Install a release package | [Install](INSTALL.md) | A verified `.deb`, `.rpm` or tarball installed, with the service left for you to enable |
| Understand the dashboard | [Dashboard](DASHBOARD.md) | Distinguish live traffic, configured objects, and stale samples |
| Reload or operate the daemon | [Operations](OPERATING.md) | Check readiness, apply a generation, and inspect drains |
| Add, remove, or evacuate workers | [Runbook](RUNBOOK.md) | Perform pool changes and recover from failures |
| Migrate native configuration | [Migration](MIGRATION.md) | Review blockers and a finalized preview before cutover |
| Publish or record live media | [RTMP](RTMP.md) | Inspect streams and recorder state within supported codec limits |
| Diagnose a failure | [Troubleshooting](TROUBLESHOOTING.md) | Collect evidence and identify the responsible layer |
| Secure management and sources | [Security](SECURITY.md) | Check token files, loopback access, and trusted inputs |
| Evaluate performance | [Benchmarks](../BENCHMARKS.md) | Interpret measured results and identify evaluation requirements |

## Before deployment

Check each required capability in the
[compatibility matrix](../COMPATIBILITY.md); protocol names and importer names do not imply complete
parity. Keep management on loopback and review [security](SECURITY.md). Local tests and historical
benchmarks do not close the production-traffic and CA-staging gates.

For exact syntax, use the [CLI](../reference/CLI.md), [configuration](../reference/CONFIGURATION.md),
and [API](../reference/API.md) references.
