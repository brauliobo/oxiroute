# Troubleshooting

Start with evidence. Do not disable health checks, authentication, or revision checks to make a
symptom disappear.

## Fast Triage

```sh
oxiroute ready
oxiroute status
oxiroute generation status
oxiroute --output json monitoring
oxiroute --output json topology
```

| Symptom | Check | Likely meaning |
| --- | --- | --- |
| `ready` returns nonzero | `/ready`, `status`, generation state | No active non-degraded generation or a traffic listener is not listening |
| Matched HTTP route returns `503` | Pool availability and endpoint health | The route exists, but no endpoint is selectable |
| Health remains `unknown` | Probe path, host, timeout, startup policy | Checks have not completed successfully enough to make the endpoint eligible |
| Dashboard keeps old values | UI stale banner and management listener logs | The last valid sample is retained after a transient refresh failure |
| Configuration save returns `409` | Disk and active revisions | Another writer changed the authoritative file; reconcile instead of overwriting |
| Configuration save returns `422` | Validation diagnostics and compositional flag | Draft or runtime preflight failed, or a compositional root cannot be flattened |
| Configuration save says restart required | Listener bind/mode changes | Active Unix listener mode changes are not live-rebound |
| Manual recorder button is disabled | Publisher, recorder phase, capability, observed codec | The requested operation is not safe or supported for this stream |
| Import produces a report but no preview | Blocking diagnostic or deployment requirement | The source contains behavior that cannot be represented safely |
| Metrics show large string counters | API shape | Decimal strings preserve exact `u64` values; do not parse through a JavaScript number |

## Origin And Routing

Verify the origin directly before inspecting proxy routing:

```sh
curl -i http://127.0.0.1:3000/healthz
curl -i http://127.0.0.1:8080/healthz
```

Then inspect the topology route and pool. Route precedence is exact host, wildcard host, catch-all;
within a class, the longest path prefix wins and source order resolves the remaining tie. A missing
route is `404`; a matched route with no selectable endpoint is `503`.

## Authentication And Files

Management token failures usually come from the token file rather than the HTTP request:

- the file must be a regular no-follow file;
- mode must be `0400` or `0600`;
- token bytes must be visible ASCII, 32 to 512 bytes after one trailing line ending is removed;
- every authenticated request must contain exactly one bearer header.

Check permissions as the daemon user. Never paste a token into logs or issue it in a shell command
that will be captured by process inspection.

## Import Failures

Use `--output report`, read blocking errors first, and keep the native source unchanged. A finalized
preview can still contain deployment warnings; reproduce user/group/chroot/logging/process behavior
outside OxiRoute before cutover.

## Collect A Useful Bug Report

Include:

- OxiRoute version and platform;
- sanitized configuration or the smallest reproducer;
- `status`, generation status, and the relevant monitoring/topology response;
- exact command and exit category;
- whether the failure is startup, validation, activation, protocol, or shutdown.

Do not include bearer tokens, private keys, certificate contents, recording roots, stream query
arguments, or unredacted native source unless the report is being handled securely.
