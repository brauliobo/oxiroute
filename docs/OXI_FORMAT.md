# OXI Configuration Format (`.oxi`)

OxiRoute's native `.oxi` format is a concise, UCI-inspired, indentation-based configuration syntax
designed for human authoring and review. It eliminates braces, brackets, commas, and unnecessary
quotes for everyday values while compiling deterministically into OxiRoute's canonical
`oxiroute_config::ValidatedConfig` (and canonical KDL) through the same unified source pipeline as
KDL 2.0, Lua 5.4, OpenWrt UCI, HOCON, YAML 1.2, and TOML (see
[`CONFIG_FORMATS.md`](CONFIG_FORMATS.md)).

## Overview and Philosophy

- **Flat, indentation-driven stanzas**: Canonical `.oxi` uses two-space indentation, LF line
  endings, and `#` comments. Sections open with a keyword and optional name (`site api`,
  `pool backend`), with child directives indented beneath them.
- **Zero syntax noise for normal values**: Paths, hostnames, IP/socket addresses, CIDR prefixes,
  globs, URLs, and identifiers do not need quotes; multi-value fields use inline space-separated
  lists (`hosts app.example.com www.example.com`).
- **Deterministic compilation**: Every `.oxi` file decodes into the bounded format-neutral value
  tree, expands cross-format `vars` and parameterized `templates`/`use`, desugars high-level
  `edge`/`sites` declarations, validates against `oxiroute_config::ConfigDraft`, and yields a
  deterministic canonical KDL representation and `candidateRevision`.
- **Full round-trip rendering**: The CLI and API support deterministic `.oxi` rendering
  (`--format oxi`), emitting canonical two-space indented stanzas with sorted keys and preserved
  array order.
- **Progressive complexity**: Start with a 10-line `edge` + `site` config, factor out repeated
  settings with cross-format `vars` and parameterized `template` blocks, split large configs across
  files with `include`, or drop down to low-level canonical stanzas and embedded sandboxed Lua 5.4
  only where needed.

## Syntax Reference

### Layout and Comments

- Indentation is significant and uses **2 spaces** per nesting level in canonical formatting.
- Blank lines are ignored.
- `#` begins a comment and runs to the end of the line (outside quoted strings). Both full-line
  and trailing inline comments are supported.
- Duplicate keys within the same object/stanza are rejected.

### Scalars

| Type | Syntax | Examples |
| --- | --- | --- |
| Boolean | `on`, `off`, `true`, `false` | `gzip on`, `http_only off`, `ipv6_only true` |
| Null | `null`, `none` | `max_connections null`, `default_cert none` |
| Integer | Base-10 signed/unsigned integers | `version 1`, `max_connections 4096`, `status 301` |
| Float | Finite decimal numbers (`inf`/`nan` rejected) | `weight 1.5`, `sample_rate 0.25` |
| Unquoted string | Single token for paths, hosts, IPs, socket addrs, globs, URLs, identifiers | `/var/www`, `app.example.com`, `*.example.com`, `127.0.0.1:3000`, `[::1]:8443`, `https://example.com` |
| Double-quoted string | `"..."` with standard escapes (`\"`, `\\`, `\n`, `\r`, `\t`) | `"Hello, world"`, `"on"`, `"123"`, `"127.0.0.1:{{port}}"` |
| Single-quoted string | `'...'` literal string without escape processing | `'raw\path'`, `'literal # text'` |

Quote a string whenever it contains whitespace, `#`, leading container syntax (`[`, `{`), or a
literal word that would otherwise parse as a boolean, null, or number (for example `"on"`,
`"null"`, or `"443"`).

### Lists and Containers

- **Inline space-separated lists**: Multiple tokens after a key decode as an array:

  ```oxi
  host app.example.com www.example.com
  alpn h2 http/1.1
  methods GET HEAD POST
  ```

- **Explicit single-item and empty containers**: Use `[...]` when a single-element list must stay
  an array rather than a scalar, `[]` for an empty array, and `{}` for an empty object:

  ```oxi
  methods [GET]
  exclude_paths []
  headers {}
  ```

- **Anonymous array items (`- `)**: Use `- ` for multi-line arrays and arrays of objects:

  ```oxi
  servers
    - address 127.0.0.1:3001
      weight 2
    - address 127.0.0.1:3002
      weight 1
  ```

### Named Stanzas

Top-level named stanzas provide a flat, readable alternative to deeply nested arrays and maps.
Each stanza keyword maps to its corresponding section in the value tree (using the stanza name as
the map key or the entry's `name` field):

| Stanza syntax | Alias | Canonical target |
| --- | --- | --- |
| `site <name>` | — | `sites.<name>` (high-level sugar) |
| `template <name>` | — | `templates.<name>` (reusable/parameterized template) |
| `pool <name>` | `upstream <name>` | `upstream_pools` entry (`name: <name>`) |
| `cert <name>` | `certificate <name>` | `certificates` entry (`name: <name>`) |
| `tls <name>` | `tls_profile <name>` | `tls_profiles` entry (`name: <name>`) |
| `service <name>` | `http_service <name>` | `http_services` entry (`name: <name>`) |
| `listener <name>` | — | `listeners` entry (`name: <name>`) |
| `cache <name>` | `cache_store <name>` | `cache_stores` entry (`name: <name>`) |
| `forward_proxy <name>` | — | `forward_proxy_services` entry (`name: <name>`) |
| `rtmp <name>` | — | `rtmp_services` entry (`name: <name>`) |
| `l4 <name>` | — | `l4_services` entry (`name: <name>`) |

Singleton top-level blocks such as `edge`, `vars`, `stats`, `management`, `audit`, `retirement`,
and `client_tracking` appear without a stanza name:

```oxi
version 1

pool api-pool
  algorithm round_robin
  servers
    - address 127.0.0.1:3001
    - address 127.0.0.1:3002

cert wildcard-example
  dns_names *.example.com example.com
  source
    type files
    certificate_chain_path /etc/oxiroute/certs/example.pem
    private_key_path /etc/oxiroute/certs/example.key
```

### Native Server Stanzas

`.oxi` supports top-level native migration references (`nginx_server`, `haproxy_server`,
`squid_server`, `apache_server`, and `varnish_server`) using the same strict options and import
contract documented in [`CONFIG_FORMATS.md`](CONFIG_FORMATS.md#native-server-references):

```oxi
nginx_server legacy-web
  path /etc/nginx/nginx.conf
  root_prefix /

haproxy_server legacy-edge
  paths /etc/haproxy/haproxy.cfg /etc/haproxy/conf.d
  node_ip 192.0.2.73
  gpu1_defined off

squid_server legacy-proxy
  path /etc/squid/squid.conf
  externalize_cache on

apache_server legacy-httpd
  path /etc/httpd/conf/httpd.conf

varnish_server legacy-cache
  path /etc/varnish/default.vcl
  arguments varnishd -a :6081 -s cache=malloc,256M
```

Native directives are extracted before template expansion and desugaring, so native files are never
generated dynamically by templates.

## High-Level `edge` and `sites` Sugar

OxiRoute provides a high-level `edge` and `sites` layer that automatically synthesizes canonical
`certificates`, `tls_profiles`, `upstream_pools`, `http_services`, and `listeners` entries during
source resolution.

> **Cross-format availability**: `edge` and `sites` desugaring runs in the unified source pipeline
> immediately after `vars`/template expansion and before typed `ConfigDraft` deserialization. It
> works identically in **`.oxi`, YAML, TOML, Lua, KDL, HOCON, and UCI**—not just `.oxi`.

### `edge` Block

The top-level `edge` block configures the shared HTTP/HTTPS listeners, default TLS settings, and
process-wide edge controls:

| Field | Description | Example |
| --- | --- | --- |
| `http` | HTTP bind address, with optional `-> https` redirect shorthand to redirect non-`http_only` traffic to HTTPS. | `http 0.0.0.0:80 -> https` or `http 0.0.0.0:80` |
| `https` | HTTPS bind address (attaches the synthesized edge TLS profile). | `https 0.0.0.0:443` |
| `min_version` | Minimum TLS protocol version for the synthesized edge TLS profile. | `min_version 1.3` (or `1.2`) |
| `alpn` | Ordered ALPN protocol list for the edge TLS profile. | `alpn h2 http/1.1` |
| `default_cert` | Default certificate name when a client omits SNI or presents an unknown hostname. | `default_cert app` |
| `stats` | Process/edge stats configuration (forwarded to canonical `stats`). | `stats` block |
| `management` | Management API configuration (forwarded to canonical `management`). | `management` block |
| `max_connections` | Connection admission cap (forwarded to canonical `max_connections`). | `max_connections 8192` |

### `site` Stanzas (`sites`)

Each `site <name>` stanza declares one virtual host (or set of hosts), its TLS certificate, its
default action, and optional path-based sub-routes:

| Field | Description | Example |
| --- | --- | --- |
| `host` / `hosts` | One or more hostnames routed to this site. | `host api.example.com` or `hosts example.com www.example.com` |
| `cert` | Certificate source: `certbot` (automatic Let's Encrypt paths), `self_signed` (development cert), explicit certificate/key file paths, or the name of an existing `cert`/`certificate` stanza to reuse. | `cert certbot`, `cert self_signed`, `cert /etc/ssl/fullchain.pem /etc/ssl/privkey.pem`, `cert wildcard-example` |
| `proxy` | Reverse-proxy target: a single backend (`host:port`), multiple backends (synthesizes a pool), or `@pool` to reference an explicit `pool`/`upstream_pools` entry. | `proxy 127.0.0.1:3000`, `proxy 127.0.0.1:3001 127.0.0.1:3002`, `proxy @api-pool` |
| `static` | Serve static files from a root directory (string path or nested options block). | `static /var/www/site` |
| `redirect` | Issue an HTTP redirect to a target URL/location (with optional status code). | `redirect https://example.com 301` |
| `respond` | Return a fixed HTTP response (status code with optional body and headers). | `respond 200 ok` |
| `route <path> -> <target>` | Arrow sub-route matched before the site's default action (see matcher forms below). | `route =/healthz -> respond 200 ok` |
| `gzip` | Enable or disable response gzip compression (`on`/`off`). | `gzip on` |
| `http_only` | Serve this site only on the HTTP listener (skips TLS cert requirement and `http ... -> https` redirect). | `http_only on` |
| `access_policy` | Site-level access control policy applied to generated routes. | `access_policy` block |
| `proxy_policy` | Site-level reverse-proxy policy (timeouts, headers, retries, etc.). | `proxy_policy` block |
| `max_request_body_bytes` | Maximum allowed request body size in bytes for the site. | `max_request_body_bytes 10485760` |

#### Arrow Sub-Routes (`route <path> -> <target>`)

Sub-routes use a path prefix sigil to pick the canonical path matcher kind:

| Pattern | Matcher kind | Behavior |
| --- | --- | --- |
| `=/exact` | `exact` | Matches only the exact request path (e.g. `route =/healthz -> respond 200 ok`). |
| `^/raw` | `raw_prefix` | Matches any path starting with the literal byte prefix (e.g. `route ^/assets -> static /var/www/assets`). |
| `/prefix` | `segment_prefix` | Matches the path segment prefix `/prefix` or `/prefix/...` (e.g. `route /api -> proxy @api-pool`). |

The right-hand side `<target>` accepts the same action forms as site-level actions (`proxy ...`,
`static ...`, `redirect ...`, `respond ...`, or a direct backend / `@pool` shorthand).

#### Using `edge` and `sites` in Other Formats (YAML Example)

Because desugaring is format-agnostic, the exact same structure works in YAML, TOML, Lua, KDL,
HOCON, or UCI:

```yaml
version: 1
edge:
  http: "0.0.0.0:80 -> https"
  https: "0.0.0.0:443"
  min_version: "1.3"
  alpn: ["h2", "http/1.1"]
sites:
  app:
    hosts: ["app.example.com"]
    cert: certbot
    proxy: "127.0.0.1:3000"
    gzip: true
    routes:
      - path: "=/healthz"
        respond: { status: 200, body: "ok" }
```

### Coexistence with Low-Level Canonical Sections

`edge` and `sites` do not lock you out of OxiRoute's full canonical schema. You can freely combine
them in the same document with explicit low-level sections:

- **Explicit `upstream_pools` (`pool`/`upstream`)** for custom load-balancing algorithms, active
  health checks, weights, or upstream TLS verification, referenced from sites via `proxy @pool`.
- **Explicit `certificates` (`cert`)** for shared wildcard certificates or custom ACME/file
  settings, referenced from sites via `cert <cert-name>`.
- **Protocol-specific services**: `rtmp_services` (`rtmp`), `forward_proxy_services`
  (`forward_proxy`), `l4_services` (`l4`), and `cache_stores` (`cache`), paired with explicit
  `listeners` (`listener`) and `http_services` (`service`) / `tls_profiles` (`tls`).

Synthesized edge objects are merged alongside explicit canonical objects before validation, so
duplicate resource names or conflicting listener bind addresses are caught immediately by standard
canonical validation. Using `edge`/`sites` sugar marks the resolved source as `compositional`.

## Cross-Format Variables (`vars`) and Parameterized Templates (`params` + `use`)

OxiRoute extends generic templates with root `vars`, template `params`, parameterized `use`
overrides, and `{{var}}` interpolation. Like `edge`/`sites` sugar, this expansion runs in the
unified source pipeline and works identically across **`.oxi`, YAML, TOML, Lua, KDL, HOCON, and
UCI**.

### Root `vars`

Declare reusable values in a top-level `vars` block. `vars` may contain scalars, nested objects,
and arrays, and is removed from the tree after expansion (it never reaches typed deserialization):

```oxi
vars
  domain example.com
  tls
    min_version 1.3
  app
    host 127.0.0.1
    port 3000
```

### Template `params` and Parameterized `use`

A `template` may declare a `params` block with default values. Callers apply the template with
`use <name>` and override any parameter inline with `k=v` pairs (or via a parameterized `use`
object in formats like YAML/TOML/JSON/KDL/Lua):

```oxi
template web-site
  params
    cert certbot
    gzip on
    port 3000
  host "{{subdomain}}.{{domain}}"
  cert "{{cert}}"
  proxy "127.0.0.1:{{port}}"
  gzip "{{gzip}}"

site app
  use web-site subdomain=app port=3001

site docs
  use web-site subdomain=docs port=3002 gzip=off
```

- **Precedence**: Template `use` argument overrides take precedence over template `params` defaults,
  which shadow root `vars`. Local fields on the using object override all inherited template fields.
- **Merging**: Multiple templates in `use` apply left to right; objects merge recursively while
  arrays, scalars, and null replace inherited values.
- **Cleanup**: Both root `vars`, root `templates`, each template's `params` block, and `use`
  markers are stripped after expansion.

### Interpolation Rules and Escaping

- **Whole-value typed interpolation**: When an entire string value is a single `{{...}}`
  placeholder (for example `"{{port}}"` or `"{{gzip}}"` or `"{{app.backends}}"`), substitution
  replaces the string with the variable's **native typed value** (integer, float, boolean, null,
  array, or object).
- **Embedded string interpolation**: When `{{...}}` appears inside a larger string (for example
  `"127.0.0.1:{{port}}"` or `"https://{{subdomain}}.{{domain}}"`), the resolved scalar is
  formatted into the string. Embedding non-scalar objects or arrays inside a string is rejected.
- **Dotted lookup**: `{{app.port}}` or `{{tls.min_version}}` traverses nested objects in scope.
- **Escaping**: Prefix with a backslash—`\{{literal}}`—to emit a literal `{{literal}}` without
  performing variable substitution.
- **Unknown variables**: Referencing an undefined variable or parameter fails resolution cleanly.

### Cross-Format Example (YAML)

```yaml
version: 1
vars:
  domain: example.com
  app:
    port: 3001
templates:
  proxied-site:
    params:
      port: 3000
    cert: certbot
    proxy: "127.0.0.1:{{port}}"
edge:
  http: "0.0.0.0:80 -> https"
  https: "0.0.0.0:443"
sites:
  api:
    use:
      template: proxied-site
      params:
        port: "{{app.port}}"
    host: "api.{{domain}}"
```

### Safety Bounds

Template expansion and variable interpolation are strictly declarative—they cannot read the
process environment, access the filesystem, or emit native server directives—and enforce hard
resource limits:

| Constant | Limit | Purpose |
| --- | --- | --- |
| `MAX_SUBSTITUTIONS` | `4,096` | Maximum `{{var}}` substitutions performed across a document. |
| `MAX_EXPANSION_DEPTH` | `64` | Maximum recursive template inheritance and expansion depth (cycles are rejected). |
| `MAX_OUTPUT_BYTES` | `4 MiB` | Maximum serialized size of the expanded value tree. |
| `MAX_STRUCTURAL_DEPTH` | `128` | Maximum nesting depth of the decoded and expanded value tree. |
| `MAX_NODES` | `400,000` | Maximum total values and object keys across the tree. |
| `MAX_STRING_BYTES` | `256 KiB` | Maximum UTF-8 byte length of any single string or object key. |

## Embedded Sandboxed Lua 5.4 Scripting

When declarative `vars` and templates are not enough (for example generating repetitive fleets of
backends, deriving deterministic hashes, or transforming external YAML/JSON inventories), `.oxi`
allows embedding sandboxed Lua 5.4 directly inside the document in three forms.

### 1. Top-Level `lua` Blocks

A top-level `lua` block evaluates a Lua 5.4 chunk that returns a table, which is merged into the
root `.oxi` document:

```oxi
vars
  base_port 4000

lua
  local backends = map(range(1, 3), function(i)
    return { address = net.join_host_port("127.0.0.1", vars.base_port + i) }
  end)
  return {
    upstream_pools = {
      oxi.upstream("workers", backends, { algorithm = "round_robin" }),
    },
  }
```

### 2. Key-Level `lua` Blocks

Attach a `lua` block directly to a key so the chunk's return value becomes that key's value:

```oxi
pool api-pool
  algorithm round_robin
  servers lua
    return map(range(1, 4), function(i)
      return { address = net.join_host_port("127.0.0.1", 8000 + i) }
    end)
```

### 3. Inline `lua(...)` Expressions

For compact single-line computations, wrap any Lua expression in `lua(...)`:

```oxi
vars
  domain example.com
  worker_port 8443

pool ipv6-backend
  servers
    - address lua(net.join_host_port("::1", vars.worker_port))
```

### Available Globals and Helpers

Embedded Lua runs in the same sandboxed Lua 5.4 runtime documented in
[`LUA_SCRIPTING.md`](LUA_SCRIPTING.md), with one extra `.oxi`-specific global:

- **`vars`**: Read-only table populated from the `.oxi` document's root `vars` block.
- **OxiRoute builder DSL (`oxi` / `oxiroute`)**: `oxi.config`, `oxi.certbot`, `oxi.cert_files`,
  `oxi.cert_self_signed`, `oxi.tls_profile`, `oxi.upstream`, `oxi.host`, `oxi.path`, `oxi.proxy`,
  `oxi.redirect`, `oxi.fixed_response`, `oxi.static_files`, `oxi.route`, `oxi.http_service`, and
  `oxi.listener`.
- **Collection and functional helpers**: `clone`, `merge`, `deep_merge`, `keys`, `values`, `map`,
  `filter`, `flat_map`, `concat`, `unique`, `contains`, `slice`, `range`, `group_by`, plus `null`,
  `array`, and `object`.
- **String, encoding, hashing, and network helpers**: `str.*`, `json.*`, `yaml.*`, `toml.*`,
  `base64.*`, `hash.sha256`, and `net.*`.
- **Directory-jailed loaders** (when resolving from disk): `include`, `read_text`, `read_lines`,
  `read_json`, `read_yaml`, and `read_toml`.

### Sandbox Guarantees

| Guarantee | Enforcement |
| --- | --- |
| Memory limit | **4 MiB** maximum Lua VM heap allocation. |
| Instruction limit | **1,000,000** VM instructions maximum (prevents infinite loops). |
| Text-only chunks | Binary Lua bytecode is rejected (`ChunkMode::Text`). |
| Blocked globals | `os`, `io`, `package`, `debug`, `coroutine`, `dofile`, `loadfile`, `load`, `require`, `collectgarbage`, `getmetatable`, `setmetatable`, `rawget`, `rawset`, `_G`, `print`, `warn` are removed (`== nil`). |
| Determinism | `math.random` and `math.randomseed` are removed; identical inputs always produce identical canonical KDL and `candidateRevision`. |

## Modular Multi-File `include` and Hot-Reload

Large configurations can be split across multiple files using top-level `include` directives.
`.oxi` can include other `.oxi` fragments as well as structured YAML, TOML, and sandboxed Lua
files:

```oxi
include pools.toml
include data.yaml
include sites/app.oxi
include dynamic.lua
```

Included files are decoded according to their extension and merged deterministically in declaration
order into the current document tree before template expansion and `edge`/`sites` desugaring.

### Security Guarantees

- **Relative paths only**: All `include` paths must be relative to the configuration root /
  including file's directory; absolute paths are rejected.
- **No parent traversal (`..`)**: Any path containing `..` is rejected so includes cannot escape
  the configuration directory jail.
- **No symlinks**: Symlinks in the target file or any intermediate directory component are
  rejected.
- **Cycle, depth, and size bounds**: Include cycles are detected and rejected, nested include depth
  is bounded, and all included inputs share the standard source, node, string, and dependency caps
  (`MAX_SOURCE_BYTES = 4 MiB`, `MAX_NODES = 400,000`, `MAX_DEPENDENCY_PATHS = 4,096`).
- **Disabled in memory-only `decode_value`**: Calling `decode_value(ConfigFormat::Oxi, bytes)`
  performs no filesystem I/O and rejects `include` directives (and embedded Lua `include`/`read_*`
  calls).

### Automatic Hot-Reload Dependency Tracking

Every file read through `.oxi` `include` (or embedded Lua `include`/`read_*`) is recorded in
`ResolvedSource.dependencies` (up to `MAX_DEPENDENCY_PATHS`, 4,096 paths). The configuration
watcher tracks these dependencies alongside the root `.oxi` file, so modifying an included site,
pool, YAML/TOML data file, or Lua module automatically triggers full re-resolution and hot reload
even when the root `.oxi` file bytes do not change.

Any root using `vars`, templates, `edge`/`sites` sugar, `include`, or native references is marked
`compositional`, which prevents typed API saves from overwriting the authored `.oxi` source
(`E_COMPOSITIONAL_ROOT`). Authored-text saves (`GET|PUT /api/v1/config/source`,
`oxiroute config apply|edit`) preserve the `.oxi` file verbatim.

## Complete Real-World Production Example

The following ~30-line `oxiroute.oxi` combines `vars`, a parameterized `template`, `edge` with
automatic HTTP-to-HTTPS redirect and TLS 1.3, multiple `site` stanzas (Certbot reverse proxy,
static assets, and an exact healthcheck route), a modular `include`, and embedded sandboxed Lua:

```oxi
version 1

vars
  domain example.com
  api_ports 3001 3002 3003

template tls-site
  params
    cert certbot
    gzip on
  cert "{{cert}}"
  gzip "{{gzip}}"
  route =/healthz -> respond 200 ok

edge
  http 0.0.0.0:80 -> https
  https 0.0.0.0:443
  min_version 1.3
  alpn h2 http/1.1
  default_cert app
  max_connections 8192

pool api-pool
  algorithm round_robin
  servers lua
    return map(vars.api_ports, function(p)
      return { address = net.join_host_port("127.0.0.1", p) }
    end)

site app
  use tls-site
  host "app.{{domain}}" "www.{{domain}}"
  proxy 127.0.0.1:4000
  route ^/static -> static /var/www/app/static

site api
  use tls-site gzip=off
  host "api.{{domain}}"
  proxy @api-pool
  max_request_body_bytes 10485760

include sites/docs.oxi
```

Validate, inspect the flattened canonical KDL, or render in `.oxi` with the CLI:

```sh
oxiroute config check oxiroute.oxi
oxiroute config compose oxiroute.oxi
oxiroute config compose --format oxi oxiroute.oxi
```
