# Configuration formats

OxiRoute accepts KDL 2.0, OXI (`.oxi`), sandboxed Lua 5.4, OpenWrt UCI, HOCON, YAML 1.2, and TOML
source documents. KDL 2.0 is the default authoring, `serve`, `config compose`, and
effective-revision format. Every source flows through the unified source pipeline and resolves to
the same strict `oxiroute_config::ConfigDraft` model before runtime preparation, including
cross-format `vars` and parameterized `templates`/`use` expansion, high-level `edge`/`sites` sugar,
and native-server references.

| Format | Current status | Contract |
| --- | --- | --- |
| KDL 2.0 | stable/default | Canonical authoring, `serve`, composition, previews, and effective revisions. |
| OXI | stable adapter | UCI-inspired indentation-based stanza syntax (`.oxi`) with inline lists, `edge`/`sites` sugar, cross-format `vars`/parameterized templates, directory-jailed `include`, embedded sandboxed Lua 5.4, and deterministic renderer. |
| Sandboxed Lua 5.4 | stable adapter | Sandboxed Lua 5.4 scripting source and deterministic renderer with generic `templates`/`use`, `vars`, `edge`/`sites` sugar, native references, and `decode_value`; blocks `os`/`io`/`package`/`debug`/`load`/`require` and enforces memory and instruction limits. |
| HOCON | stable adapter | Declarative source and renderer with bounded substitutions and native references; no environment discovery or include execution. |
| OpenWrt UCI | stable adapter | Declarative reversible mapping and renderer; no shell execution, anonymous sections, or unsupported list forms. |
| YAML 1.2 | stable adapter | Strict YAML 1.2 source (`.yaml`, `.yml`) and deterministic minimalist renderer with `vars`, parameterized `templates`/`use`, `edge`/`sites` sugar, and native references; rejects aliases, anchors, and multi-document streams. |
| TOML | stable adapter | Standard TOML source (`.toml`) and deterministic renderer with `vars`, parameterized `templates`/`use`, `edge`/`sites` sugar, and native references; TOML has no `null` type. |

## Source pipeline

The source pipeline has one semantics regardless of syntax:

1. Read one stable, bounded UTF-8 root snapshot without following a root-file symlink.
2. Infer syntax from the path: no extension, `.kdl`, or `.kdl2` means KDL; `.oxi` means OXI;
   `.lua` means sandboxed Lua; `.uci` means UCI; `.hocon` or `.conf` means HOCON; `.yaml` or `.yml`
   means YAML 1.2; and `.toml` means TOML. Matching is case-insensitive.
3. Decode a bounded generic value tree while rejecting duplicate object keys and ambiguous
   constructs (including evaluating directory-jailed `include` directives and embedded Lua in `.oxi`
   or `.lua` when resolved from disk). Extract strict top-level native-server declarations.
4. Expand root `vars`, generic and parameterized `templates` (`params`), `use` markers, and
   `{{var}}` interpolation. Objects merge recursively, arrays and scalars replace, local fields
   override templates, and cycles or excessive substitutions are rejected.
5. Desugar high-level `edge` and `sites` declarations (when present in any format) into canonical
   `certificates`, `tls_profiles`, `upstream_pools`, `http_services`, and `listeners` entries.
6. Deserialize any inline fragment into `oxiroute_config::ConfigDraft` and resolve each `nginx_server`,
   `haproxy_server`, `squid_server`, `apache_server`, and `varnish_server` through the existing
   complete native import pipeline.
7. Compose the inline fragment first and imported fragments in declaration order. Reject conflicting
   process-wide values, duplicate identities, dangling references, and every non-final native import.
8. Apply schema defaults, normalize, validate, and render deterministic KDL. The SHA-256 of those KDL
   bytes is the candidate/runtime revision.

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

A `[::]` socket bind may add `ipv6_only #true` beside `address` to serve IPv6 clients alone. The default is
dual-stack; `ipv6_only` is then omitted from every rendered format and accepted in all of them.

Canonical KDL uses two-space indentation, LF endings, sorted object keys, preserved array order, and
no statement-ending commas or semicolons. Properties, typed scalar arguments, untyped child blocks,
non-finite numbers, duplicate nodes, and KDL 1 booleans such as `true` are rejected. KDL 2 booleans
and null are `#true`, `#false`, and `#null`.

Files in exactly the canonical rendering are decoded by a dedicated single-pass reader; any other
KDL (comments, `;` separators, raw or escaped strings beyond `\"`, `\\`, `\n`, `\r`, `\t`, `\b`,
`\f`, CRLF endings, tabs) goes through the generic KDL parser, which also produces every syntax
diagnostic. Both paths accept the same documents and produce the same values.

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

The root `templates` and `vars` objects (along with template `params` blocks and `use` markers) are
removed after expansion and never reach typed deserialization. Templates also support declarative
variables and parameterization across every supported source format (`.oxi`, KDL, Lua, HOCON, UCI,
YAML, and TOML):

- **Root `vars` and template `params`**: Define global values under root `vars` and per-template
  defaults under `params`, then override parameters on parameterized `use` declarations (for
  example `use web-site port=3001` in `.oxi` or parameterized `use` objects in other formats).
- **`{{var}}` interpolation**: Whole-value placeholders (`"{{port}}"`) preserve the variable's
  native type, while embedded placeholders (`"127.0.0.1:{{port}}"`) interpolate scalars into
  strings. Dotted lookup (`{{app.port}}`) traverses nested objects, and `\{{` emits a literal `{{`.
- **Cross-format `edge`/`sites` sugar**: After template and variable expansion, any format may also
  declare high-level `edge` and `sites` blocks that desugar into canonical listeners, TLS profiles,
  certificates, upstream pools, and HTTP services alongside low-level sections.

Template expansion cannot inspect the process environment, read files, or create native directives:
native declarations are extracted before expansion, and expansion is bounded by
`MAX_SUBSTITUTIONS` (4,096), `MAX_EXPANSION_DEPTH` (64), and `MAX_OUTPUT_BYTES` (4 MiB). See
[`OXI_FORMAT.md`](OXI_FORMAT.md) for the complete `vars`, parameterized template, and `edge`/`sites`
sugar reference.

## OXI

OXI sources (`.oxi`) use a UCI-inspired, flat, two-space indentation stanza syntax designed for
concise human authoring—with zero braces, brackets, commas, or quotes required for normal values,
`#` comments, inline space-separated lists (`hosts app.example.com www.example.com`), explicit
containers (`[GET]`, `[]`, `{}`), `- ` array items, and named stanzas (`site`, `template`,
`pool`/`upstream`, `cert`/`certificate`, `tls`/`tls_profile`, `service`/`http_service`, `listener`,
`cache`/`cache_store`, `forward_proxy`, `rtmp`, `l4`, and native server stanzas).

Built-in `.oxi` capabilities include:

- **High-level `edge` and `sites` sugar**: Concise HTTP/HTTPS listener, `-> https` redirect,
  Certbot/self-signed/file/reused certificates, `@pool` proxying, and arrow sub-routes
  (`route =/healthz -> respond 200 ok`), coexisting cleanly with low-level canonical sections.
- **Cross-format `vars` and parameterized templates**: Root `vars`, template `params`, inline
  `use <name> k=v` overrides, and typed/string `{{var}}` interpolation.
- **Embedded sandboxed Lua 5.4**: Top-level `lua` blocks (with a read-only `vars` global and the
  full `oxi` DSL and helpers), key-level `lua` blocks, and inline `lua(...)` expressions under the
  same 4 MiB / 1M-instruction deterministic sandbox as `.lua` files.
- **Modular multi-file `include` and hot reload**: Directory-jailed relative `include` for `.oxi`,
  `.yaml`, `.toml`, and `.lua` files (no `..`, no symlinks, cycle/depth/size bounded, disabled in
  memory-only `decode_value`) with automatic dependency tracking in `ResolvedSource.dependencies`.
- **Deterministic `.oxi` rendering**: `config compose --format oxi` and API rendering emit canonical
  two-space indented `.oxi`.

See [`OXI_FORMAT.md`](OXI_FORMAT.md) for the complete `.oxi` syntax reference, `edge`/`sites`
sugar guide, cross-format `vars`/parameterized templates specification, embedded Lua rules, and
production examples.

## YAML 1.2

YAML sources (`.yaml`, `.yml`) are parsed as strict YAML 1.2 into the bounded generic value tree.
Aliases, anchors, and multi-document streams are rejected so documents remain self-contained and
unambiguous.

YAML supports generic `templates`/`use` expansion and top-level native server references using the
same object or array-of-objects shapes as HOCON. Rendering emits deterministic minimalist YAML with
sorted object keys and preserved array order.

## TOML

TOML sources (`.toml`) follow standard TOML syntax and decode into the bounded generic value tree.
TOML supports generic `templates`/`use` expansion and top-level native server references using the
same table or array-of-tables shapes as HOCON.

Rendering emits deterministic TOML with sorted keys and preserved array order. Note that TOML has no
`null` type.

## Lua

Lua sources (`.lua`) run in a sandboxed, deterministic Lua 5.4 environment (text-only chunks with
memory, instruction, and tree-size limits) and return one configuration table. The sandbox exposes
`table`, `string`, `math` (without `random` or `randomseed`), `utf8`, allowlisted safe base
functions, and `null`/`array`/`object` value helpers, while blocking `os`, `io`, `package`, `debug`,
`coroutine`, `load`, `require`, and other unsafe globals.

Built-in scripting capabilities include:

- **Collection, string, encoding, hashing, and network helpers**: functional table utilities
  (`clone`, `merge`, `deep_merge`, `map`, `filter`, `flat_map`, `group_by`, etc.), `str.*`,
  `json`/`yaml`/`toml`/`base64`/`hash.sha256`, and `net.*`.
- **OxiRoute builder DSL (`oxi` / `oxiroute`)**: concise constructors such as `oxi.config`,
  `oxi.certbot`, `oxi.tls_profile`, `oxi.upstream`, `oxi.route`, `oxi.proxy`, `oxi.http_service`,
  and `oxi.listener`.
- **Modular includes and data loading**: directory-jailed `include` and
  `read_text`/`read_lines`/`read_json`/`read_yaml`/`read_toml` with automatic hot-reload dependency
  tracking via `ResolvedSource.dependencies` (disabled in memory-only `decode_value`/`load_lua`).
- **Full source pipeline composition**: seamless support for generic `templates`/`use` expansion,
  top-level native server references (using the same table shapes as HOCON), `decode_value`, and
  deterministic Lua rendering.

See [`LUA_SCRIPTING.md`](LUA_SCRIPTING.md) for the complete Lua 5.4 scripting reference, sandbox
bounds, DSL guide, and examples.

## Native server references

Native references make an existing validated configuration the migration source of truth. They are
top-level source declarations, not fields in the canonical typed model. Options that cannot be
inferred safely remain explicit:

```kdl
version 1

nginx_server "/etc/nginx/nginx.conf" {
  root_prefix "/"
  host_timezone "America/Argentina/Buenos_Aires"
  default_access_log_file "/var/lib/oxiroute/http-access.jsonl"
  recording_root "/mnt/cloud/4tb/cam-rtmp"
  default_error_server "nginx/1.30.2"
  bearer_token_file "api.example.com" "/run/secrets/api.token"
  upstream_tls "192.0.2.44:8443" "origin.example.com" "/etc/oxiroute/origin-ca.pem"
}

haproxy_server "/etc/haproxy/haproxy.cfg" "/etc/haproxy/conf.d" {
  node_ip "192.0.2.73"
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
The two repeatable nginx options `bearer_token_file "<server_name>" "<path>"` and
`upstream_tls "<authority>" "<sni>" ["<ca>"]` are the only exception: each occurrence takes its
positional string arguments (no properties, types, or children), and every other option still rejects duplicates.

HOCON (as well as YAML, TOML, and Lua) uses a single object or an array of objects with these exact shapes:

```hocon
nginx_server = {
  path = "nginx.conf"
  root_prefix = "."
  bearer_token_file = [{ server_name = "api.example.com", path = "/run/secrets/api.token" }]
  upstream_tls = [{ authority = "192.0.2.44:8443", server_name = "origin.example.com", ca_certificate_path = "/etc/oxiroute/origin-ca.pem" }]
}
haproxy_server = [{
  paths = ["frontend.cfg", "backend.cfg"]
  node_ip = "192.0.2.73"
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

UCI uses named source sections. `nginx_server`, `squid_server`, and `apache_server` accept scalar `option` entries
(nginx also accepts the repeatable `list bearer_token_file '<server_name>=<path>'` and
`list upstream_tls '<authority>=<sni>[,ca=<path>]'` entries);
`haproxy_server` uses ordered `list path` entries and optional scalar preprocessing values; Varnish uses
one scalar `option path` and ordered `list arguments` entries:

```uci
config nginx_server 'web'
  option path 'nginx.conf'
  option root_prefix '.'
  list bearer_token_file 'api.example.com=/run/secrets/api.token'
  list upstream_tls '192.0.2.44:8443=origin.example.com,ca=/etc/oxiroute/origin-ca.pem'

config haproxy_server 'edge'
  list path 'frontend.cfg'
  list path 'backend.cfg'
  option node_ip '192.0.2.73'
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
`default_access_log_file` (migrates omitted and formatted nginx access logs), `recording_root`,
`default_error_server`, `x_accel_controls_absent`, and the repeatable `bearer_token_file` and
`upstream_tls`. `bearer_token_file` binds a literal-bearer `if ($http_authorization != ...)` gate to an
operator-provisioned token file (regular file, mode 0400 or 0600, one printable-ASCII line; the importer never
reads or stores the nginx literal) and is identified by the case-insensitive `server_name`. `upstream_tls`
hardens one https `proxy_pass` authority with a verified SNI and optional CA bundle; without it https upstreams
keep nginx's unverified default. Both lists reject empty values and duplicate identities, and an entry that
matches no lowered rule fails the import. UCI cannot express the CLI-only `activate` flag. Relative token and CA paths resolve from the source document directory. Apache reference options are
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
oxiroute config compose edge.kdl sites.oxi legacy.lua openwrt.uci site.conf routes.yaml policy.toml
oxiroute config compose --format oxi edge.kdl legacy.lua
oxiroute config compose --format lua edge.kdl legacy.lua
oxiroute config compose --format uci edge.kdl legacy.lua
oxiroute config compose --format hocon edge.kdl legacy.lua
oxiroute config compose --format yaml edge.kdl legacy.lua
oxiroute config compose --format toml edge.kdl legacy.lua
```

The default output is KDL; `--format` accepts `kdl`, `oxi`, `lua`, `uci`, `hocon`, `yaml`, or
`toml`. Composition output does not retain comments, `vars`, templates, `edge`/`sites` sugar,
`include` structure, HOCON substitutions, UCI record names, native declarations, or provenance. It
is the explicit flattening operation.

Standalone `import nginx|haproxy --emit preview` also defaults to KDL and accepts
`--format kdl|oxi|lua|uci|hocon|yaml|toml` for a fully finalized candidate. `--format` does not
alter `report` output, which remains evidence rather than a configuration source.

## Format policy

- OXI (`.oxi`) decodes flat, two-space indentation stanzas, inline space-separated lists, explicit
  containers, named stanzas, optional directory-jailed `include` directives (when resolved from
  disk), and optional embedded sandboxed Lua 5.4 blocks/expressions into the bounded generic value
  tree, then participates in cross-format `vars`/template expansion and `edge`/`sites` desugaring.
  Deterministic `.oxi` rendering emits canonical two-space stanzas.
- Lua executes in a sandboxed, text-only, memory- and instruction-bounded Lua 5.4 runtime. It
  exposes deterministic `table`, `string`, `math` (without `random`/`randomseed`), `utf8`, safe base
  functions (`pairs`, `ipairs`, `next`, `type`, `tostring`, `tonumber`, `select`, `pcall`, `xpcall`,
  `error`, `assert`), and `null`/`array` helpers, while rejecting binary chunks and blocking `os`,
  `io`, `package`, `debug`, `load`, and `require`.
- YAML 1.2 is parsed strictly without aliases, anchors, or multi-document streams, and rendered
  deterministically in minimalist YAML form.
- TOML is parsed as standard TOML and rendered deterministically; note that TOML has no `null` type.
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
- Upstream `tls.verify` (`"full"` or `"none"`, with an optional `tls.server_name`) and the cookie
  rewrite `mode` (`"exact"` or `"nginx_prefix"`) are plain canonical fields in every format. The
  Lua renderer always writes both; the KDL, OXI, UCI, HOCON, YAML, and TOML renderers omit the
  default value.
- No format infers process environment values. HAProxy preprocessing inputs are explicit source
  fields and participate in the effective candidate revision.

## Security and Save Limits

- Source bytes and rendered output are capped at 4 MiB. Generic trees are capped at 128 structural
  levels, 400,000 nodes, 256 KiB per string, 64 template inheritance levels, 4,096 variable/HOCON
  substitutions, and 4,096 recorded dependency paths.
- Declarative parsing itself performs no implicit I/O. Native references and on-disk `.oxi`/`.lua`
  directory-jailed `include`/`read_*` loaders are the explicit exceptions: native references read
  the named roots and importer-defined include graph using the daemon account's filesystem
  permissions without invoking native binaries or a shell, while `.oxi`/`.lua` includes are
  confined strictly to relative non-symlink paths inside the config directory (and disabled in
  memory-only `decode_value`). Referenced files must be trusted local administrator input.
- Native failures returned through the source coordinator are redacted to importer identity and
  stable diagnostic-code counts. Use offline import reports for detailed migration diagnostics.
- API drafts are typed JSON and are rendered in the root path's format. A typed save normalizes
  formatting and cannot preserve source constructs. For that reason the backend refuses to replace
  any root marked compositional by `vars`, templates, `edge`/`sites` sugar, `include` dependencies,
  or native references (`E_COMPOSITIONAL_ROOT`).
- Authored-text saves (`GET|PUT /api/v1/config/source`, `config apply|edit`) store the submitted bytes in
  the format of the daemon's own file, so comments, formatting, Lua logic, templates and native references
  survive; a text in another format is refused (`E_FORMAT_MISMATCH`). `config fmt` and `config compose`
  render the typed result and do not preserve comments or the author's ordering. Inline RTMP token secrets
  and callback URL credentials are shown as `<redacted:...>` placeholders in the stored text the API returns
  and are restored when the text is submitted back; a text the daemon cannot redact safely is withheld.
- The browser consumes format-preserving previews and can save a non-compositional root in any
  supported syntax. It disables typed editing/save controls for compositional roots while retaining
  inspection and server validation.
