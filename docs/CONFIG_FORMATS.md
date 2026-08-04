# Configuration formats

OxiRoute accepts KDL 2.0, restricted Lua, OpenWrt UCI, and HOCON source documents. KDL 2.0 is the
default authoring, `serve`, `config compose`, and effective-revision format. Every source resolves to
the same strict `oxiroute_config::Config` model before runtime preparation. Lua remains supported for
existing deployments, but generic templates and native-server references are available only in the
declarative KDL/HOCON/UCI pipeline.

| Format | Current status | Contract |
| --- | --- | --- |
| KDL 2.0 | stable/default | Canonical authoring, `serve`, composition, previews, and effective revisions. |
| Restricted Lua | partial adapter | Supported text-only compatibility source and renderer; not the canonical format and does not support generic templates or native references. |
| HOCON | stable adapter | Declarative source and renderer with bounded substitutions and native references; no environment discovery or include execution. |
| OpenWrt UCI | stable adapter | Declarative reversible mapping and renderer; no shell execution, anonymous sections, or unsupported list forms. |

## Source pipeline

The source pipeline has one semantics regardless of syntax:

1. Read one stable, bounded UTF-8 root snapshot without following a root-file symlink.
2. Infer syntax from the path: no extension, `.kdl`, or `.kdl2` means KDL; `.lua` means restricted
   Lua; `.uci` means UCI; and `.hocon` or `.conf` means HOCON. Matching is case-insensitive.
3. For KDL/HOCON/UCI, decode a bounded generic value tree while rejecting duplicate object keys and
   ambiguous constructs. Extract strict top-level native-server declarations.
4. Expand generic root `templates` and exact object `use` markers. Objects merge recursively, arrays
   and scalars replace, local fields override templates, and cycles are rejected.
5. Deserialize any inline fragment into `oxiroute_config::Config` and resolve each `nginx_server`,
   `haproxy_server`, `squid_server`, `apache_server`, and `varnish_server` through the existing
   complete native import pipeline.
6. Compose the inline fragment first and imported fragments in declaration order. Reject conflicting
   process-wide values, duplicate identities, dangling references, and every non-final native import.
7. Apply schema defaults, normalize, validate, and render deterministic KDL. The SHA-256 of those KDL
   bytes is the candidate/runtime revision.

Lua takes the legacy direct path: the bounded evaluator returns one table, which is immediately
decoded and validated as `Config`, then rendered to deterministic KDL for its candidate revision.
Lua does not pass through generic templates or native reference extraction.

`diskRevision` remains the SHA-256 of the exact authored root bytes and is used for revision-checked
saves. `candidateRevision` identifies the effective runtime generation and changes whenever
templates, inline values, defaults, or native references change. A native-only change can therefore
change `candidateRevision` while leaving `diskRevision` unchanged.

## KDL 2.0

KDL is parsed as KDL 2.0 only. KDL 1 fallback is not enabled. Scalars are nodes with one argument;
objects and arrays use explicit type annotations so empty and single-item collections remain
unambiguous:

```kdl
version 1
(array)listeners {
  (object)- {
    name "web"
    (object)bind {
      type "socket"
      address "0.0.0.0:80"
    }
    protocol "http"
    service "web"
  }
}
```

Canonical KDL uses two-space indentation, LF endings, sorted object keys, preserved array order, and
no statement-ending commas or semicolons. Properties, typed scalar arguments, untyped child blocks,
non-finite numbers, duplicate nodes, and KDL 1 booleans such as `true` are rejected. KDL 2 booleans
and null are `#true`, `#false`, and `#null`.

## Generic Templates

Templates are generic object data, not a second configuration schema or executable text. A template
may use another template and may be applied to any object with `use`. `use` may also be an array of
names: templates apply left to right, later templates override earlier ones, and local fields win.
Object values merge recursively; arrays, scalars, and null replace inherited values.

```kdl
(object)templates {
  (object)edge-listener {
    max_connections 4096
    (object)downstream_timeouts {
      client_timeout_ms 50000
      request_timeout_ms 10000
      keepalive_timeout_ms 10000
    }
  }
}

(array)listeners {
  (object)- {
    use "edge-listener"
    name "public"
    protocol "http"
  }
}
```

The root `templates` object is removed after expansion and never reaches typed deserialization.
Templates cannot interpolate strings, inspect the environment, read files, or create native
directives: native declarations are extracted before expansion. Template expansion is available in
KDL, HOCON, and generic UCI records, not restricted Lua.

## Native server references

Native references make an existing validated configuration the migration source of truth. They are
top-level source declarations, not fields in the canonical typed model. Options that cannot be
inferred safely remain explicit:

```kdl
version 1

nginx_server "/etc/nginx/nginx.conf" {
  root_prefix "/"
  host_timezone "America/Bahia"
  default_access_log_file "/var/lib/oxiroute/http-access.jsonl"
  recording_root "/mnt/cloud/4tb/cam-rtmp"
  default_error_server "nginx/1.30.2"
}

haproxy_server "/etc/haproxy/haproxy.cfg" "/etc/haproxy/conf.d" {
  node_ip "10.0.0.11"
  gpu1_defined #false
}

squid_server "/etc/squid/squid.conf" {
  externalize_cache #true
}

apache_server "/etc/httpd/conf/httpd.conf"

varnish_server "/etc/varnish/default.vcl" "varnishd" "-a" ":6081" "-s" "cache=malloc,256M"
```

KDL permits repeated native server nodes. nginx, Squid, and Apache take exactly one positional path;
HAProxy takes one or more ordered positional paths; Varnish takes one positional path followed by
optional ordered invocation arguments. Child options must be untyped scalar nodes, and properties are rejected.

HOCON uses a single object or an array of objects with these exact shapes:

```hocon
nginx_server = {
  path = "nginx.conf"
  root_prefix = "."
}
haproxy_server = [{
  paths = ["frontend.cfg", "backend.cfg"]
  node_ip = "10.0.0.11"
  gpu1_defined = false
}]
squid_server = {
  path = "squid.conf"
  externalize_cache = true
}
apache_server = {
  path = "httpd.conf"
}
varnish_server = {
  path = "default.vcl"
  arguments = ["varnishd", "-a", ":6081", "-s", "cache=malloc,256M"]
}
```

UCI uses named source sections. `nginx_server`, `squid_server`, and `apache_server` accept scalar `option` entries;
`haproxy_server` uses ordered `list path` entries and optional scalar preprocessing values; Varnish uses
one scalar `option path` and ordered `list arguments` entries:

```uci
config nginx_server 'web'
  option path 'nginx.conf'
  option root_prefix '.'

config haproxy_server 'edge'
  list path 'frontend.cfg'
  list path 'backend.cfg'
  option node_ip '10.0.0.11'
  option gpu1_defined '0'

config squid_server 'proxy'
  option path 'squid.conf'
  option externalize_cache '1'

config apache_server 'web'
  option path 'httpd.conf'

config varnish_server 'cache'
  option path 'default.vcl'
  list arguments 'varnishd'
  list arguments '-a'
  list arguments ':6081'
  list arguments '-s'
  list arguments 'cache=malloc,256M'
```

nginx reference options are exactly `path`, `root_prefix`, `host_timezone`,
`default_access_log_file`, `recording_root`, and `default_error_server`. Apache reference options are
exactly `path`. HAProxy reference options are
exactly ordered `paths`, optional `node_ip`, and `gpu1_defined`; true `gpu1_defined` requires
`node_ip`. Squid accepts `path` and `externalize_cache`; the latter is required when parsed refresh
rules would otherwise be discarded by direct non-caching forwarding. Relative paths, including an explicitly relative nginx `root_prefix`, resolve from the
OxiRoute source document directory. Varnish accepts `path` and ordered `arguments`; unknown options,
empty paths, native blockers, or an invalid
composed namespace reject the complete OxiRoute source; partial native candidates are never
activated.

The watcher observes the OxiRoute root's parent and periodically performs a complete re-resolution,
which detects effective native changes even when the root bytes do not change. It does not rewrite
native files.

## CLI Rendering

`config check` and `serve` infer each source's syntax from its path. `config compose` may mix input
formats and flattens every input to one typed configuration:

```sh
oxiroute config compose edge.kdl legacy.lua openwrt.uci site.conf
oxiroute config compose --format lua edge.kdl legacy.lua
oxiroute config compose --format uci edge.kdl legacy.lua
oxiroute config compose --format hocon edge.kdl legacy.lua
```

The default output is KDL; `--format` accepts `kdl`, `lua`, `uci`, or `hocon`. Composition output does
not retain comments, templates, HOCON substitutions, UCI record names, native declarations, or
provenance. It is the explicit flattening operation.

Standalone `import nginx|haproxy --output preview` also defaults to KDL and accepts
`--format kdl|lua|uci|hocon` for a fully finalized candidate. `--format` does not alter `report`
output, which remains evidence rather than a configuration source.

## Format policy

- Lua retains its restricted, text-only, instruction-bounded compatibility behavior. It loads no
  standard libraries, rejects binary chunks, and exposes no filesystem, process, network, package,
  dynamic-load, or debug facilities.
- HOCON supports local substitutions and object merging with an empty environment. Optional process
  substitutions such as `${?HOME}` disappear; required `${HOME}` remains unresolved and rejects the
  source. Every include form is rejected before resolution. Deterministic HOCON rendering is sorted,
  pretty JSON, which HOCON accepts.
- UCI's reversible mapping uses named `config json` records. `root` has `option kind 'object'`; every
  child names its `parent`, an object `key` or contiguous array `index`, a scalar/container `kind`,
  and an encoded scalar `value` when needed. Anonymous sections, duplicate declarations/options,
  malformed parent graphs, sparse arrays, unknown section types, and unsupported lists are rejected.
  The parser treats UCI as bytes to decode, never as a shell program.
- A friendly named `config oxiroute 'main'` section may provide only scalar `version` and
  `max_connections`; other canonical fields use generic records. Native sections use the exact
  `option`/`list` mapping above.
- No format infers process environment values. HAProxy preprocessing inputs are explicit source
  fields and participate in the effective candidate revision.

## Security and Save Limits

- Source bytes and rendered output are capped at 1 MiB. Generic trees are capped at 128 structural
  levels, 100,000 nodes, 256 KiB per string, 64 template inheritance levels, and 4,096 recorded
  dependency paths.
- Declarative parsing itself performs no implicit I/O. Native references are the explicit exception:
  they read the named roots and importer-defined include graph using the daemon account's filesystem
  permissions. They do not invoke native binaries or a shell. Referenced files must be trusted local
  administrator input.
- Native failures returned through the source coordinator are redacted to importer identity and
  stable diagnostic-code counts. Use offline import reports for detailed migration diagnostics.
- API drafts are typed JSON and are rendered in the root path's format. A typed save normalizes
  formatting and cannot preserve source constructs. For that reason the backend refuses to replace
  any root marked compositional by templates or native references (`E_COMPOSITIONAL_ROOT`).
- The browser consumes format-preserving previews and can save a non-compositional root in any
  supported syntax. It disables typed editing/save controls for compositional roots while retaining
  inspection and server validation.
