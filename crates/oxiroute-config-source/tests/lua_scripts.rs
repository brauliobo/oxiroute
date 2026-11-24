use std::path::Path;

use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, LuaConfigError, MAX_SOURCE_BYTES, MAX_STRING_BYTES,
    MAX_STRUCTURAL_DEPTH, decode_value, load_lua, resolve_source_with_format,
};
use serde_json::json;

#[test]
fn lua_scripting_supports_stdlib_helpers_loops_and_config_generation() {
    let script = r#"
        local function make_cert(name, host)
            return {
                name = name,
                dns_names = { host },
                source = {
                    type = "files",
                    certificate_chain_path = string.format("/etc/oxiroute/%s-chain.pem", name),
                    private_key_path = string.format("/etc/oxiroute/%s-key.pem", name),
                },
            }
        end

        local function make_route(prefix, pool)
            local clean = string.gsub(prefix, "//+", "/")
            return {
                path = { kind = "segment_prefix", value = clean },
                action = { type = "proxy", upstream_pool = pool, policy = {} },
            }
        end

        local names = { "beta", "alpha" }
        table.sort(names)

        local certificates = {}
        local listeners = {}
        local routes = {}

        for idx, name in ipairs(names) do
            local host = table.concat({ name, "example.test" }, ".")
            table.insert(certificates, make_cert(name, host))
            table.insert(listeners, {
                name = string.format("http-%s", name),
                bind = {
                    type = "socket",
                    address = string.format("127.0.0.1:%d", 8080 + math.floor(math.max(idx, 1))),
                },
                protocol = "http",
                service = "web",
            })
            table.insert(routes, make_route("//" .. name, "backend"))
        end

        local ok, label_len = pcall(utf8.len, "oxiroute-π")
        assert(ok and label_len == 10, "utf8.len failed")

        local count = 0
        for _k, _v in pairs({ a = 1, b = 2 }) do
            count = count + 1
        end
        assert(count == 2, "pairs count mismatch")

        return {
            version = 1,
            certificates = certificates,
            listeners = listeners,
            upstream_pools = {
                {
                    name = "backend",
                    endpoints = {
                        { type = "dns", host = "origin.example.test", port = 443 },
                    },
                },
            },
            http_services = {
                {
                    name = "web",
                    routes = routes,
                },
            },
        }
    "#;

    let loaded = load_lua(script).expect("load_lua should evaluate scripting helpers");
    assert_eq!(loaded.as_draft().certificates.len(), 2);
    assert_eq!(loaded.as_draft().certificates[0].name, "alpha");
    assert_eq!(loaded.as_draft().certificates[1].name, "beta");
    assert_eq!(loaded.as_draft().listeners.len(), 2);
    assert_eq!(loaded.as_draft().http_services[0].routes.len(), 2);

    let resolved = resolve_source_with_format(
        Path::new("generated.lua"),
        script.as_bytes(),
        ConfigFormat::Lua,
    )
    .expect("resolve_source_with_format should evaluate Lua script");
    assert_eq!(resolved.config, loaded);
}

#[test]
fn lua_scripting_is_deterministic_without_rng() {
    for expr in ["return math.random()", "return math.randomseed(1)"] {
        let error = decode_value(ConfigFormat::Lua, expr.as_bytes()).unwrap_err();
        assert!(
            matches!(error, ConfigSourceError::Lua(LuaConfigError::Lua(_))),
            "expected Lua runtime error for `{expr}`, got {error:?}"
        );
    }

    let check = decode_value(
        ConfigFormat::Lua,
        b"return { r = math.random == nil, s = math.randomseed == nil }",
    )
    .expect("checking nil RNG fields");
    assert_eq!(check, json!({ "r": true, "s": true }));
}

#[test]
fn lua_sandbox_blocks_dangerous_globals_and_bytecode() {
    let blocked_globals = [
        "os",
        "io",
        "package",
        "debug",
        "coroutine",
        "dofile",
        "loadfile",
        "load",
        "require",
        "collectgarbage",
        "getmetatable",
        "setmetatable",
        "rawget",
        "rawset",
        "_G",
        "print",
        "warn",
    ];

    for global in blocked_globals {
        let check_nil = format!("return {{ blocked = ({global} == nil) }}");
        let decoded = decode_value(ConfigFormat::Lua, check_nil.as_bytes())
            .unwrap_or_else(|err| panic!("checking `{global} == nil` failed: {err}"));
        assert_eq!(
            decoded,
            json!({ "blocked": true }),
            "global `{global}` must not be exposed"
        );

        let call_expr = format!("return {global}()");
        let err = decode_value(ConfigFormat::Lua, call_expr.as_bytes()).unwrap_err();
        assert!(
            matches!(err, ConfigSourceError::Lua(LuaConfigError::Lua(_))),
            "calling `{global}()` must fail with Lua error, got {err:?}"
        );
    }

    // Lua bytecode header (\x1bLua...) must be rejected by ChunkMode::Text.
    let bytecode = b"\x1bLua\x54\x00";
    let err = decode_value(ConfigFormat::Lua, bytecode).unwrap_err();
    assert!(matches!(err, ConfigSourceError::Lua(LuaConfigError::Lua(_))));
}

#[test]
fn lua_decode_supports_array_helper_null_and_shared_dags() {
    let script = r#"
        local shared = { enabled = true, note = null }
        return {
            empty_list = array({}),
            empty_obj = {},
            explicit_list = array({ 10, 20 }),
            implicit_list = { "a", "b" },
            left = shared,
            right = shared,
        }
    "#;

    let decoded = decode_value(ConfigFormat::Lua, script.as_bytes()).expect("decode Lua");
    assert_eq!(
        decoded,
        json!({
            "empty_list": [],
            "empty_obj": {},
            "explicit_list": [10, 20],
            "implicit_list": ["a", "b"],
            "left": { "enabled": true, "note": null },
            "right": { "enabled": true, "note": null },
        })
    );
}

#[test]
fn lua_decode_rejects_cycles_mixed_tables_sparse_arrays_and_invalid_values() {
    let cases = [
        ("direct cycle", "local t = {}; t.self = t; return t"),
        (
            "indirect cycle",
            "local a = {}; local b = { a = a }; a.b = b; return a",
        ),
        ("mixed keys", r#"return { "a", key = "b" }"#),
        ("sparse array", r#"return { [1] = "a", [3] = "c" }"#),
        ("zero index", r#"return { [0] = "a" }"#),
        ("negative index", r#"return { [-1] = "a" }"#),
        ("array with string keys", r#"return array({ key = "v" })"#),
        ("non-finite infinity", "return { x = 1 / 0 }"),
        ("non-finite nan", "return { x = 0 / 0 }"),
        ("function value", "return { f = function() return 1 end }"),
        ("boolean key", "return { [true] = 1 }"),
    ];

    for (label, script) in cases {
        let err = decode_value(ConfigFormat::Lua, script.as_bytes()).unwrap_err();
        assert!(
            matches!(err, ConfigSourceError::Parse { format: "Lua", .. }),
            "case `{label}` should return Parse(\"Lua\"), got {err:?}"
        );
    }

    let bad_array_arg = decode_value(ConfigFormat::Lua, b"return array(123)").unwrap_err();
    assert!(matches!(
        bad_array_arg,
        ConfigSourceError::Lua(LuaConfigError::Lua(_))
    ));
}

#[test]
fn lua_decode_enforces_structural_depth_node_string_source_and_vm_bounds() {
    // Structural depth > MAX_STRUCTURAL_DEPTH.
    let depth_script = format!(
        "local t = {{}}; for i = 1, {} do t = {{ inner = t }} end; return t",
        MAX_STRUCTURAL_DEPTH + 2
    );
    let err = decode_value(ConfigFormat::Lua, depth_script.as_bytes()).unwrap_err();
    assert!(
        matches!(err, ConfigSourceError::StructuralDepth),
        "expected StructuralDepth, got {err:?}"
    );

    // Node limit > MAX_NODES via a compact DAG (2^20 leaves > 400_000 nodes).
    let node_script = "local t = { leaf = 1 }; for i = 1, 20 do t = { l = t, r = t } end; return t";
    let err = decode_value(ConfigFormat::Lua, node_script.as_bytes()).unwrap_err();
    assert!(
        matches!(err, ConfigSourceError::NodeLimit),
        "expected NodeLimit, got {err:?}"
    );

    // String limit > MAX_STRING_BYTES.
    let string_script = format!(
        r#"return {{ big = string.rep("a", {}) }}"#,
        MAX_STRING_BYTES + 1
    );
    let err = decode_value(ConfigFormat::Lua, string_script.as_bytes()).unwrap_err();
    assert!(
        matches!(err, ConfigSourceError::StringLimit),
        "expected StringLimit, got {err:?}"
    );

    // Source limit > MAX_SOURCE_BYTES.
    let oversized = vec![b' '; MAX_SOURCE_BYTES + 1];
    let err = decode_value(ConfigFormat::Lua, &oversized).unwrap_err();
    assert!(matches!(err, ConfigSourceError::SourceTooLarge));

    // Instruction limit.
    let err = decode_value(ConfigFormat::Lua, b"while true do end").unwrap_err();
    assert!(matches!(
        err,
        ConfigSourceError::Lua(LuaConfigError::Lua(_))
    ));

    // Memory limit.
    let mem_script = r#"local t = {}; for i = 1, 100000 do t[i] = string.rep("x", 1024) end; return t"#;
    let err = decode_value(ConfigFormat::Lua, mem_script.as_bytes()).unwrap_err();
    assert!(matches!(
        err,
        ConfigSourceError::Lua(LuaConfigError::Lua(_))
    ));
}

#[test]
fn lua_supports_templates_and_use_expansion_via_resolve_source() {
    let script = r#"
        return {
            version = 1,
            templates = {
                http_listener = {
                    protocol = "http",
                    service = "web",
                },
            },
            listeners = {
                {
                    use = "http_listener",
                    name = "edge",
                    bind = { type = "socket", address = "127.0.0.1:8080" },
                },
            },
            upstream_pools = {
                {
                    name = "origin",
                    endpoints = {
                        { type = "dns", host = "origin.example.test", port = 443 },
                    },
                },
            },
            http_services = {
                {
                    name = "web",
                    routes = {
                        {
                            path = { kind = "segment_prefix", value = "/" },
                            action = { type = "proxy", upstream_pool = "origin", policy = {} },
                        },
                    },
                },
            },
        }
    "#;

    let resolved = resolve_source_with_format(
        Path::new("templated.lua"),
        script.as_bytes(),
        ConfigFormat::Lua,
    )
    .expect("Lua templates should expand and resolve");

    assert!(resolved.compositional);
    assert_eq!(resolved.format, ConfigFormat::Lua);
    assert_eq!(resolved.config.as_draft().listeners.len(), 1);
    assert_eq!(resolved.config.as_draft().listeners[0].name, "edge");
    assert_eq!(
        resolved.config.as_draft().listeners[0].service.as_deref(),
        Some("web")
    );
}
