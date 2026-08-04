# Configuration Reference

OxiRoute accepts one canonical typed model through several bounded source adapters. KDL 2.0 is the
recommended format for new files.

## Choose A Format

| Path | Adapter | Important boundary |
| --- | --- | --- |
| `site.kdl`, `site.kdl2`, or `site` | KDL 2.0 | Templates and native references are available |
| `site.lua` | Restricted Lua | Legacy compatibility; no generic templates or native references |
| `site.hocon` or `site.conf` | HOCON | Declarative; includes and required environment substitutions are rejected |
| `site.uci` | OpenWrt UCI mapping | Data-only; generic records preserve the typed tree |

Every source is bounded, decoded, normalized, cross-reference checked, and rendered into a complete
runtime plan. A syntax parser accepting a value does not make its runtime semantics supported.

## Smallest Useful HTTP Shape

```kdl
version 1

(array)upstream_pools {
  (object)- {
    name "web"
    algorithm "round_robin"
    (array)servers {
      (object)- {
        name "origin-1"
        (object)endpoint {
          type "socket"
          address "127.0.0.1:3000"
        }
      }
    }
  }
}

(array)http_services {
  (object)- {
    name "web"
    (array)routes {
      (object)- {
        (object)action {
          type "proxy"
          upstream_pool "web"
          (object)policy {
          }
        }
        (object)path {
          kind "segment_prefix"
          value "/"
        }
        (object)policy {
          max_request_body_bytes 10485760
          request_buffering #false
          response_buffering #false
        }
      }
    }
  }
}

(array)listeners {
  (object)- {
    name "web"
    protocol "http"
    service "web"
    (object)bind {
      type "socket"
      address "127.0.0.1:8080"
    }
  }
}
```

Use [oxiroute.example.kdl](../../oxiroute.example.kdl) for a complete example with management,
TCP, RTMP, health checks, and explicit defaults.

## Common Rules

- Names are unique within each typed collection.
- Listener binds and normalized endpoints cannot overlap in unsafe ways.
- DNS names remain stable canonical identities; resolution happens at startup or connect time as
  configured and is bounded.
- Pools select only eligible, healthy, administratively ready, and capacity-available endpoints.
- Route precedence is exact host, wildcard host, catch-all; then longest segment path and source order.
- HTTP retry budgets are bounded to three additional attempts and obey method/body/connection safety.
- Unix paths, recording roots, certificate files, token files, and static roots have descriptor-safe
  and ownership/mode checks.

## Composition And Save

Templates and native references make a root compositional. The resolver can validate and render a
candidate, but the typed API/UI refuses to save a flattened replacement over that source. Use
`config compose` when you explicitly want one canonical flattened file:

```sh
oxiroute config compose edge.kdl legacy.lua --format kdl > composed.kdl
oxiroute config check composed.kdl
```

Read [CONFIG_SPEC.md](../CONFIG_SPEC.md) for the full schema and [CONFIG_FORMATS.md](../CONFIG_FORMATS.md)
for deterministic syntax mappings.
