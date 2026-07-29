# Configuration formats

OxiRoute accepts KDL 2.0, restricted Lua, OpenWrt UCI, and HOCON source documents. KDL 2.0 is the
default authoring and canonical preview format. Every format is decoded into the same bounded value
tree before template expansion, native-server resolution, typed deserialization, normalization, and
canonical validation.

## Source pipeline

The source pipeline has one semantics regardless of syntax:

1. Read one bounded UTF-8 root snapshot without following the configured root path.
2. Select the parser from an explicit format or the `.kdl`, `.lua`, `.uci`, or `.conf` extension.
3. Decode a bounded value tree while rejecting duplicate object keys and ambiguous constructs.
4. Expand declarative `templates` and exact `use` references. Objects merge recursively, arrays
   replace, local fields override template fields, and inheritance cycles are rejected.
5. Resolve `nginx_server` and `haproxy_server` references through the existing complete native
   import pipelines. Native files remain read-only.
6. Compose inline and imported fragments in source order.
7. Deserialize the result into `oxiroute_config::Config`, apply defaults, and validate it.
8. Render deterministic KDL and hash those bytes as the resolved runtime revision.

The root revision remains the SHA-256 of the exact authored bytes and is used for revision-checked
saves. The resolved revision identifies runtime generations and changes whenever the effective
configuration changes, including changes discovered through native references.

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
no statement-ending commas or semicolons.

## Declarative reuse

Templates are typed data, not executable text. A template may use another template and may be
applied to any object with `use`. Templates cannot execute code, interpolate strings, inspect the
environment, or read files. Expansion depth, node count, and rendered size are bounded.

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

## Native server references

Native references make an existing validated configuration the migration source of truth. Options
that cannot be inferred safely remain explicit:

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
```

Relative native paths resolve from the source document directory. Native import blockers reject the
complete OxiRoute source; partial native candidates are never activated.

## Format policy

- Lua retains its existing restricted, instruction-bounded, data-only compatibility behavior.
- HOCON supports local substitutions and object merging with an empty environment. Built-in file,
  URL, classpath, and package includes are rejected so all I/O remains visible to OxiRoute.
- UCI accepts deterministic named sections and rejects anonymous sections, duplicate options, and
  shell execution. Native-server sections use ordinary `option` and `list` entries.
- No format may infer process environment values. HAProxy preprocessing inputs are explicit source
  fields and are included in the resolved revision.
