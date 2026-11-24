# Lua 5.4 Scripting

OxiRoute embeds a sandboxed, deterministic Lua 5.4 runtime for authoring `.lua` configuration files.
A Lua source script evaluates to a single configuration table that flows through the same unified
source pipeline as KDL, HOCON, UCI, YAML, and TOML—including generic `templates`/`use` expansion,
native server references, schema validation, and deterministic rendering (see
[`CONFIG_FORMATS.md`](CONFIG_FORMATS.md)).

## Sandbox and Determinism

Lua scripts run in a fresh, isolated Lua 5.4 VM per load and are destroyed immediately after
decoding. Binary bytecode chunks are rejected (`ChunkMode::Text`), and non-deterministic or
side-effectful facilities are removed so identical inputs always produce identical canonical KDL and
candidate revisions.

### Resource Bounds

| Bound | Limit | Purpose |
| --- | --- | --- |
| VM memory | 4 MiB | Caps total Lua heap allocations during evaluation. |
| VM instructions | 1,000,000 | Prevents infinite loops and runaway computation. |
| `MAX_SOURCE_BYTES` | 4 MiB | Caps root source text size (and rendered output size). |
| `MAX_STRUCTURAL_DEPTH` | 128 | Caps table/array nesting depth in the decoded value tree. |
| `MAX_NODES` | 400,000 | Caps total values and object keys across the decoded tree (including expanded DAGs). |
| `MAX_STRING_BYTES` | 256 KiB | Caps UTF-8 byte length of any single string value or object key. |

### Available Standard Libraries and Base Globals

- **Standard libraries**: `table`, `string`, `math` (with `math.random` and `math.randomseed`
  removed), and `utf8`.
- **Allowlisted base functions**: `assert`, `error`, `ipairs`, `next`, `pairs`, `pcall`, `xpcall`,
  `select`, `tonumber`, `tostring`, `type`, `rawequal`, `rawlen`.
- **Value-shaping helpers**:
  - `null` — explicit null value (whereas Lua `nil` omits a table field).
  - `array(tbl)` — marks a table as an explicit array (required to distinguish an empty array `[]`
    from an empty object `{}` in `decode_value` and rendered output; typed config deserialization
    also accepts `{}` for sequence fields).
  - `object(tbl)` — marks a table as an explicit object/map.

### Blocked Globals

The following Lua globals are removed from the sandbox environment (`== nil`) and cannot be called:

`os`, `io`, `package`, `debug`, `coroutine`, `dofile`, `loadfile`, `load`, `require`,
`collectgarbage`, `getmetatable`, `setmetatable`, `rawget`, `rawset`, `_G`, `print`, `warn`.

### Table and Value Conversion Rules

- Tables must contain either string keys (object) or contiguous 1-based positive integer keys
  (array); mixed keys, sparse arrays, zero/negative indices, and non-string/non-integer keys are
  rejected.
- Cyclic tables, non-finite numbers (`inf`, `-inf`, `nan`), functions/userdata values in the
  returned tree, and non-UTF-8 strings are rejected.
- Shared acyclic table references (DAGs) are permitted and expanded within `MAX_NODES`.

## Built-in Scripting Helpers

In addition to the allowlisted Lua 5.4 standard libraries, the sandbox registers deterministic
helpers for data transformation, string handling, encoding, hashing, and networking.

### Collection and Functional Helpers

| Function | Description |
| --- | --- |
| `clone(value)` | Deep-copies a table or returns a scalar unchanged. |
| `merge(a, b, ...)` | Shallow-merges object tables left to right; later keys override earlier ones. |
| `deep_merge(a, b, ...)` | Recursively merges nested object tables; arrays and scalars replace. |
| `keys(tbl)` | Returns an array of object keys in deterministic sorted order. |
| `values(tbl)` | Returns an array of object values in deterministic key order. |
| `map(list, fn)` | Applies `fn(item, index)` to each element and returns a new array. |
| `filter(list, fn)` | Returns a new array containing elements where `fn(item, index)` is truthy. |
| `flat_map(list, fn)` | Maps each element with `fn` and flattens one level of returned arrays. |
| `concat(a, b, ...)` | Concatenates two or more arrays into a new array. |
| `unique(list)` | Returns a new array with duplicate values removed, preserving first-seen order. |
| `contains(list, value)` | Returns `true` if `list` contains `value`. |
| `slice(list, start, stop)` | Returns a sub-array from 1-based `start` to `stop` (inclusive). |
| `range(start, stop, step)` | Generates an integer sequence array (`step` defaults to `1`). |
| `group_by(list, fn)` | Groups array items into an object keyed by `tostring(fn(item))`. |

### String Helpers (`str.*`)

| Function | Description |
| --- | --- |
| `str.trim(s)` | Strips leading and trailing ASCII/Unicode whitespace. |
| `str.split(s, sep)` | Splits `s` on literal separator `sep` into an array of strings. |
| `str.starts_with(s, prefix)` | Returns `true` if `s` starts with `prefix`. |
| `str.ends_with(s, suffix)` | Returns `true` if `s` ends with `suffix`. |
| `str.replace(s, from, to)` | Replaces all literal occurrences of `from` with `to`. |
| `str.join(list, sep)` | Joins an array of strings with `sep`. |

### Encoding and Hashing

| Module / Function | Description |
| --- | --- |
| `json.encode(val)` / `json.decode(s)` | Encodes Lua values to JSON and decodes JSON strings into Lua values. |
| `yaml.encode(val)` / `yaml.decode(s)` | Encodes Lua values to YAML 1.2 and decodes strict YAML 1.2 strings. |
| `toml.encode(val)` / `toml.decode(s)` | Encodes Lua tables to TOML and decodes TOML strings (TOML has no `null`). |
| `base64.encode(s)` / `base64.decode(s)` | Standard Base64 encoding and decoding. |
| `base64.encode_url(s)` / `base64.decode_url(s)` | URL-safe Base64 encoding and decoding. |
| `hash.sha256(s)` | Returns the lowercase hex-encoded SHA-256 digest of `s`. |

### Network Helpers (`net.*`)

| Function | Description |
| --- | --- |
| `net.join_host_port(host, port)` | Formats `host:port`, automatically bracketing IPv6 addresses (e.g. `[::1]:8443`). |
| `net.is_ipv4(s)` | Returns `true` if `s` is a valid IPv4 address. |
| `net.is_ipv6(s)` | Returns `true` if `s` is a valid IPv6 address. |
| `net.is_ip(s)` | Returns `true` if `s` is a valid IPv4 or IPv6 address. |
| `net.is_cidr(s)` | Returns `true` if `s` is a valid IPv4 or IPv6 CIDR prefix. |
| `net.cidr_contains(cidr, ip)` | Returns `true` if `ip` falls inside `cidr`. |
| `net.normalize_host(host)` | Normalizes a hostname (lowercased, trimmed, trailing dot stripped). |

```lua
local backends = map(range(1, 3), function(i)
  return { name = "app-" .. i, address = net.join_host_port("::1", 3000 + i) }
end)
local tag = string.sub(hash.sha256(json.encode(backends)), 1, 8)
```

## OxiRoute Builder DSL (`oxi` / `oxiroute`)

The `oxi` global (also aliased as `oxiroute`) provides concise constructors for common OxiRoute
configuration objects. Every builder returns a plain Lua table matching the canonical
`oxiroute_config::ConfigDraft` schema, so DSL output can be inspected, transformed with `merge` /
`map`, or mixed freely with hand-written tables.

### Builder Reference

| Builder | Purpose |
| --- | --- |
| `oxi.config(spec)` | Builds a root configuration table with `version = 1` and optional sections (`certificates`, `tls_profiles`, `upstream_pools`, `http_services`, `listeners`, etc.). |
| `oxi.certbot(name, dns_names, opts)` | Builds a Certbot certificate entry (`source.type = "certbot"`), deriving standard `/etc/letsencrypt/{live,archive}/<domain>` paths when omitted. |
| `oxi.cert_files(name, dns_names, chain_path, key_path)` | Builds a file-backed certificate entry (`source.type = "files"`). |
| `oxi.cert_self_signed(name, dns_names, opts)` | Builds a labeled self-signed development certificate entry. |
| `oxi.tls_profile(name, certs, opts)` | Builds a `tls_profiles` entry with certificate list, `default_certificate`, `min_version`, and `alpn`. |
| `oxi.upstream(name, servers, opts)` | Builds an `upstream_pools` entry with servers/endpoints, load-balancing algorithm, health check, or upstream TLS options. |
| `oxi.host(hostname)` | Builds a normalized host matcher (`{ kind = "normalized_host", value = ... }`). |
| `oxi.path(prefix, kind)` | Builds a path matcher (defaults to `segment_prefix`, e.g. `{ kind = "segment_prefix", value = prefix }`). |
| `oxi.proxy(pool, policy)` | Builds a `proxy` route action targeting `upstream_pool = pool`. |
| `oxi.redirect(location, status)` | Builds a `redirect` route action with target location and HTTP status code. |
| `oxi.fixed_response(status, body, opts)` | Builds a `fixed_response` route action with status code, body, and optional headers/content type. |
| `oxi.static_files(root, opts)` | Builds a `static_files` route action serving files from `root`. |
| `oxi.route(spec)` | Builds an HTTP route combining `host`, `path`, `methods`, and `action`. |
| `oxi.http_service(name, routes, opts)` | Builds an `http_services` entry from a list of routes and optional service settings. |
| `oxi.listener(name, bind, protocol, service, opts)` | Builds a `listeners` entry (accepting a socket address string or bind table, plus optional `tls_profile`, `max_connections`, etc.). |

### Example: Multi-Domain Certbot + TLS + Reverse Proxy

```lua
local sites = {
  { name = "app", host = "app.example.com", backend = "127.0.0.1:3001" },
  { name = "api", host = "api.example.com", backend = "127.0.0.1:3002" },
}

return oxi.config({
  certificates = map(sites, function(s) return oxi.certbot(s.name, { s.host }) end),
  tls_profiles = {
    oxi.tls_profile("edge-tls", map(sites, function(s) return s.name end)),
  },
  upstream_pools = map(sites, function(s) return oxi.upstream(s.name, { s.backend }) end),
  http_services = {
    oxi.http_service("edge", map(sites, function(s)
      return oxi.route({ host = oxi.host(s.host), path = oxi.path("/"), action = oxi.proxy(s.name) })
    end)),
  },
  listeners = {
    oxi.listener("https", "0.0.0.0:443", "http", "edge", { tls_profile = "edge-tls" }),
  },
})
```

## Modular Multi-File Configs and External Data Loading

When a Lua config is resolved from disk (`serve`, `config check`, `config compose`, `resolve_source`),
the sandbox exposes read-only, directory-jailed helpers for splitting large configs into modules and
loading external data files:

| Function | Description |
| --- | --- |
| `include(path)` | Evaluates a relative `.lua` file (e.g. `include("routes/api.lua")`) in the same sandbox and returns its result. |
| `read_text(path)` | Reads a relative UTF-8 text file and returns its contents as a string. |
| `read_lines(path)` | Reads a relative UTF-8 text file and returns an array of lines. |
| `read_json(path)` | Reads and decodes a relative JSON file into Lua values. |
| `read_yaml(path)` | Reads and decodes a relative strict YAML 1.2 file into Lua values. |
| `read_toml(path)` | Reads and decodes a relative TOML file into Lua values. |

```lua
local domains = read_yaml("data/domains.yaml")
local api_routes = include("routes/api.lua")
local allowlist = filter(map(read_lines("data/allowlist.txt"), str.trim), function(line)
  return line ~= "" and not str.starts_with(line, "#")
end)
```

### Security Guarantees

- **Config-directory jail**: All paths must be relative to the root configuration file's parent
  directory. Absolute paths and parent-directory traversal (`..`) are rejected.
- **No symlinks**: Symlinks in the target or intermediate path components are rejected so modules
  cannot escape the configuration directory.
- **Cycle, depth, and size bounds**: Recursive `include` cycles are detected and rejected, nested
  include depth is bounded, and every file read is subject to source/string/node/dependency limits.
- **Disabled in memory-only evaluation**: `include` and `read_*` are disabled in memory-only
  entry points (`decode_value` and `load_lua`), where no on-disk configuration directory exists.

### Automatic Hot-Reload Dependency Tracking

Every file loaded via `include` or `read_*` is automatically recorded in
`ResolvedSource.dependencies` (up to `MAX_DEPENDENCY_PATHS`, 4,096 paths). The configuration
watcher tracks these dependencies alongside the root `.lua` file, so editing an included module or
external JSON/YAML/TOML data file triggers re-resolution and hot reload even when the root file
bytes do not change.

Like sources that use generic templates or native server references, a Lua file with external
dependencies is marked `compositional`, which prevents typed API saves from overwriting the authored
script (`E_COMPOSITIONAL_ROOT`). Authored-text saves (`PUT /api/v1/config/source`,
`config apply|edit`) continue to preserve the Lua source verbatim.

## Composition with Generic Templates and Native References

Lua scripts participate in the full unified source pipeline. A script can emit root `templates` and
`use` markers (expanded after Lua evaluation) and top-level native server references
(`nginx_server`, `haproxy_server`, `squid_server`, `apache_server`, `varnish_server`) using the same
object or array-of-objects shapes documented in [`CONFIG_FORMATS.md`](CONFIG_FORMATS.md):

```lua
local extra_routes = include("routes/edge.lua")

return {
  version = 1,
  templates = {
    ["edge-listener"] = {
      max_connections = 4096,
      downstream_timeouts = {
        client_timeout_ms = 50000,
        request_timeout_ms = 10000,
        keepalive_timeout_ms = 10000,
      },
    },
  },
  listeners = {
    {
      use = "edge-listener",
      name = "public",
      bind = { type = "socket", address = "0.0.0.0:80" },
      protocol = "http",
      service = "edge",
    },
  },
  upstream_pools = {
    oxi.upstream("app", { "127.0.0.1:3000" }),
  },
  http_services = {
    oxi.http_service("edge", extra_routes),
  },
  nginx_server = {
    path = "legacy/nginx.conf",
    root_prefix = ".",
  },
  haproxy_server = {
    {
      paths = { "legacy/frontend.cfg", "legacy/backend.cfg" },
      node_ip = "192.0.2.73",
      gpu1_defined = false,
    },
  },
}
```

Validate or flatten a Lua configuration into canonical KDL (or any other supported format) with the
CLI:

```sh
oxiroute config check edge.lua
oxiroute config compose edge.lua
oxiroute config compose --format lua edge.lua
```
