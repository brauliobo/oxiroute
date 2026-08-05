# Migration Guide

OxiRoute imports are evidence-producing, fail-closed adapters. A successful parser run is not the
same as an equivalent runtime. Always inspect the report and the compatibility matrix before a cutover.

## The Safe Sequence

1. Preserve the native source tree and record its include order and deployment assumptions.
2. Run an offline `report` first.
3. Resolve every blocking diagnostic and decide how to reproduce deployment-only behavior outside
   OxiRoute.
4. Request a deterministic `preview` only after the candidate is fully finalized.
5. Run `config check` on the preview in the target host environment.
6. Validate a side-by-side shadow bind where possible.
7. Keep the native service available until traffic, health, TLS, logging, and rollback are verified.

Native files are read-only inputs. OxiRoute does not rewrite them.

## nginx

Use the machine-readable report to see the product/capability profile, bounded include graph,
per-source fingerprints, source locations, candidate provenance, unsupported directives, blockers,
and deployment requirements:

```sh
oxiroute import nginx /etc/nginx/nginx.conf \
  --root-prefix / \
  --output report
```

For a fully representable candidate, render KDL (the default) or another supported format:

```sh
oxiroute import nginx /etc/nginx/nginx.conf \
  --root-prefix / \
  --host-timezone UTC \
  --default-access-log-file /var/lib/oxiroute/http-access.jsonl \
  --output preview
```

The complete-root path retains process/events/module/error-log deployment requirements and can
compose the strict nginx-RTMP listener/application/recording subset in the same candidate. The
strict HTTP fragment API is intentionally narrower than a complete nginx installation. Variables,
regex locations, rewrites, module-specific policy, and non-equivalent TLS or cache behavior block
affected services instead of disappearing. Supported named RTMP recorder blocks retain their names;
unsupported recorder fields remain blocking.

## HAProxy

Ordered `-f` inputs and explicit preprocessing values matter. OxiRoute never discovers native
environment variables implicitly:

```sh
oxiroute import haproxy /etc/haproxy/haproxy.cfg \
  --node-ip 10.0.0.15 \
  --gpu1-defined \
  --output report
```

Preview a finalized candidate and shift imported IP listeners for a side-by-side check:

```sh
oxiroute import haproxy /etc/haproxy/haproxy.cfg \
  --node-ip 10.0.0.15 \
  --shadow-port-offset 10000 \
  --output preview
```

HAProxy logging, process identity, chroot, and worker topology are deployment requirements, not
silently activated daemon behavior. Complex ACLs, unsupported stats/authentication forms, and
non-equivalent redispatch semantics remain blocking.

## Squid

The current subset targets direct authenticated HTTP/1 forwarding. Cache and refresh semantics are
not silently converted into a non-caching runtime:

```sh
oxiroute import squid /etc/squid/squid.conf --output report
oxiroute import squid /etc/squid/squid.conf --output preview
```

If refresh rules are present, explicit `externalize_cache` acceptance is required before a direct
candidate can activate. CONNECT, ACL ordering, bounded DNS, and Basic authentication have dedicated
policy constraints. Static ordered `cache_peer <host> parent <http-port> 0` entries without options
are lowered for HTTP/1, together with global `always_direct allow all` or `never_direct allow all`
fallback rules. Sibling, dynamic, credentialed, hierarchy, ICP, and peer-option forms remain
blocking; full Squid parity is not claimed.

## Apache httpd

The Apache importer accepts a deliberately narrow static reverse-proxy subset. Start with a report:

```sh
oxiroute import apache /etc/httpd/conf/httpd.conf --output report
```

Preview a finalized candidate on a shadow port range:

```sh
oxiroute import apache /etc/httpd/conf/httpd.conf \
  --shadow-port-offset 10000 \
  --output preview
```

`Listen`, exact `VirtualHost` names, static `ProxyPass`, equal-weight `balancer://` pools, bounded
`Include`/`IncludeOptional`, and TLS certificate/key paths are covered. Rewrites, `ProxyPassMatch`,
`ProxyPassReverse`, directory/location merges, dynamic balancer state, and unsupported modules are
blocking. Apache source references use `apache_server "..."` in KDL or the equivalent HOCON/UCI
object and remain read-only compositional inputs.

## Composition And Native References

Use `config compose` to flatten finalized inputs into one typed file. It is an explicit lossy boundary:
templates, native declarations, comments, HOCON substitutions, UCI record names, and provenance do
not survive as source constructs.

```sh
oxiroute config compose edge.kdl legacy.lua openwrt.uci site.conf
oxiroute config compose --format hocon edge.kdl legacy.lua
```

KDL, HOCON, and UCI may instead declare strict `nginx_server`, `haproxy_server`, `squid_server`,
`apache_server`, or `varnish_server` references. Varnish references may include repeated explicit
`arguments` for the `varnishd` invocation facts. Those roots remain compositional: the browser can inspect and validate them, but typed
save refuses to flatten them accidentally. A KDL/HOCON/UCI native reference re-resolves its complete
source graph during watcher reconciliation; it is native-source integration, not an import management
API or UI workflow.

## Cutover Checklist

- `config check` passes on the target host with the service account.
- Every listener bind is intentional and does not collide with management or stats.
- Health checks have a known startup state and expected path/status.
- Certificate names, SNI defaults, upstream verification, and ALPN policies are tested.
- Access logs and native deployment requirements are reproduced outside the import.
- The candidate has a rollback path and the previous generation remains observable.
- Unsupported constructs are documented as an operational decision, not assumed equivalent.
