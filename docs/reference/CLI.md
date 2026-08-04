# CLI Reference

The `oxiroute` binary is both the daemon and the offline/operator client. `oxr` is the packaged
short alias. A lone configuration path remains an alias for `serve CONFIG`.

## Global Options

```sh
oxiroute [--endpoint URL] [--token-file FILE] [--output table|json|plain] [--quiet] COMMAND
```

- `--endpoint` defaults to `OXIROUTE_ENDPOINT`, then `http://127.0.0.1:9900`.
- `--token-file` explicitly selects the token file and wins over all automatic discovery.
- `OXIROUTE_MANAGEMENT_TOKEN_FILE` is the next source; when unset, the packaged
  `/etc/oxiroute/oxiroute.env` assignment is checked, followed by the built-in
  `/etc/oxiroute/management.token` path when that file exists.
- `--output json` writes one JSON value to stdout; diagnostics use stderr.
- Public `/ready` and `/metrics` probes do not need the management token. Other management operations
  use the bearer token according to their route contract.

Squid report output includes `capabilities`, a versioned target-checkout registry. Its `parity` is
`partial` and `completeParity` is false while any family or directive is partial, unsupported,
obsolete, or not planned. Only forms with both runtime integration and failure coverage are marked
`compatible`.

## Command Families

| Family | Commands | Use |
| --- | --- | --- |
| Process | `serve`, `status`, `ready`, `metrics`, `monitoring`, `topology`, `drain`, `shutdown` | Start and inspect the daemon |
| Lifecycle | `generation status|reload|rollback|drain` | Prepare, publish, revert, or drain generations |
| Configuration | `config check|compose|get|diff|validate|apply` | Inspect and change canonical configuration |
| Events | `events list|follow` | Poll the bounded in-memory event ring |
| Runtime inventory | `listener`, `pool`, `server` | Read or safely mutate admission/health state |
| TLS | `tls list|reconcile` | Inspect identities and invoke external-lineage reconciliation |
| RTMP | `rtmp stream`, `rtmp recorder`, `rtmp relay` | Inspect live sessions and supported recorder actions |
| Migration | `import nginx|haproxy|squid|apache` | Emit offline reports or finalized previews |
| Build | `version` | Print the build version |

## Output And Exit Categories

Stable exit categories are designed for scripts:

| Code | Category |
| --- | --- |
| `2` | CLI usage |
| `3` | Local input or token file |
| `4` | Transport/connectivity |
| `5` | Authentication |
| `6` | Missing resource |
| `7` | Revision or state conflict |
| `8` | Remote/protocol failure |
| `9` | Intentionally unsupported |

Use JSON output for automation and treat `409`/exit `7` as a reconcile decision, not as permission to
retry an old mutation.

## Examples

```sh
oxiroute config check /etc/oxiroute/oxiroute.kdl
oxiroute --output json server show --pool public-v4 --pool public-v6 origin-a
oxiroute server drain --pool public-v4 --pool public-v6 origin-a
oxiroute generation rollback
oxiroute import nginx /etc/nginx/nginx.conf --output report
oxiroute import squid /etc/squid/squid.conf --output report
oxiroute import apache /etc/httpd/conf/httpd.conf --output report
oxiroute import apache /etc/httpd/conf/httpd.conf --shadow-port-offset 10000 --output preview
```

See [MANAGEMENT_CLI.md](../MANAGEMENT_CLI.md) for every subcommand, capability status, revision
guarantee, and HAProxy intent mapping.
