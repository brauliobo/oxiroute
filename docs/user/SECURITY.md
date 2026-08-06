# Security Boundaries

The safest deployment assumes that configuration files, native references, certificate paths,
recording roots, and management tokens are administrator-owned inputs.

## Management Exposure

- The management listener is loopback-only in the current schema.
- Every recognized management/API route requires the file-backed bearer token loaded at startup,
  except exact `GET /ready` and `GET /metrics` probes.
- The UI keeps that token in page memory and does not persist it.
- Monitoring, topology, RTMP, generation, TLS, process, event, and recorder-control routes use the
  same bearer boundary as configuration writes. The management listener remains loopback-only; do
  not expose it remotely.
- Public statistics binds expose only `/ready` and `/metrics` without the statistics token. Restricted
  statistics reads and mutations require loopback plus the configured token and revision.

## Secret Files

Management and statistics token files must be regular no-follow files with restrictive modes. Create
them with a restrictive umask and grant only the daemon/client identities that need them:

```sh
umask 077
openssl rand -hex 32 > /etc/oxiroute/management.token
chmod 600 /etc/oxiroute/management.token
```

The CLI uses the first available explicit, configured, or packaged/default path and relies on the
operating system's read permission. It does not require the file to be owned by the invoking user;
root or the configured daemon/operator identity must simply have read and directory-search access.
The accepted token modes remain exactly `0400` and `0600`. The packaged environment file is read
only for the bounded `OXIROUTE_MANAGEMENT_TOKEN_FILE=/path` assignment; it is never executed as a
shell script.

Do not put tokens in source control, URLs, screenshots, Remotion props, support bundles, or process
arguments.

## Configuration Trust

KDL, HOCON, and UCI are bounded declarative formats. Lua is text-only and runs without standard
libraries, filesystem, process, network, package, dynamic-load, or debug facilities. Native
references are an explicit exception: they read named nginx, HAProxy, Apache, Squid, or Varnish source
graphs with the daemon's filesystem permissions. They do not invoke a shell or native binary.

## Network Policy

The forward-proxy path resolves destinations and applies policy to the complete answer to prevent
DNS-based bypasses. Private and special-use destinations are denied by default where configured.
Opaque TCP relay is not a firewall, NAT, transparent interceptor, or source-spoofing facility.

## Redaction

Management responses and topology omit private keys, credentials, token material, recording roots,
and stream query arguments. Access logs use a fixed redacted JSONL shape. Keep external diagnostics
and imported source reports under the same access controls as the original configuration.

Read [API_UI_SPEC.md](../API_UI_SPEC.md), [CONFIG_FORMATS.md](../CONFIG_FORMATS.md), and the
[packaging notes](../../packaging/arch/README.md) for exact file ownership and deployment rules.
