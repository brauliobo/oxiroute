# User Guides

These pages answer operator questions in the order they usually occur: get a safe local instance
running, understand what the dashboard reports, operate changes, then migrate existing proxy
configuration.

## Recommended Sequence

1. [Getting started](GETTING_STARTED.md) runs a local HTTP path from origin to listener.
2. [Dashboard](DASHBOARD.md) explains what is live telemetry and what is configuration state.
3. [Operations](OPERATING.md) covers readiness, generations, drains, and runtime controls.
4. [Migration](MIGRATION.md) explains how to test native configuration without rewriting it.
5. [RTMP](RTMP.md) covers live media and recorder-specific behavior.
6. [Troubleshooting](TROUBLESHOOTING.md) gives evidence-first recovery steps.
7. [Security](SECURITY.md) should be read before binding anything beyond a private host.

## Before Production

OxiRoute is pre-alpha. Confirm each required feature in [COMPATIBILITY.md](../COMPATIBILITY.md),
run the relevant wire and failure-path tests, and keep management listeners on loopback. The current
runtime is a useful narrow proxy and observability stack, not a drop-in replacement for nginx,
HAProxy, Squid, or a firewall.
