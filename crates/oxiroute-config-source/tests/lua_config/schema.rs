use crate::common::{changed, lua_error_from, with_web_pool_fields};

#[test]
fn rejects_unknown_fields_including_the_old_direct_upstream() {
    let source = changed(
        "      service = \"web\",",
        "      service = \"web\",\n      upstream = \"127.0.0.1:3000\",",
    );
    let error = lua_error_from(&source);

    assert!(matches!(
        error,
        oxiroute_config_source::LuaConfigError::Lua(_)
    ));
    assert!(error.to_string().contains("unknown field `upstream`"));
}

#[test]
fn rejects_unknown_fields_in_tls_and_http_version_objects() {
    let cases = [
        changed(
            "      name = \"web-certificate\",",
            "      name = \"web-certificate\",\n      unexpected = true,",
        ),
        changed(
            "        type = \"files\",",
            "        type = \"files\",\n        unexpected = true,",
        ),
        changed(
            "      name = \"web-tls\",",
            "      name = \"web-tls\",\n      unexpected = true,",
        ),
        with_web_pool_fields(
            r#"      tls = { server_name = "backend.example.com", unexpected = true },"#,
        ),
        with_web_pool_fields(
            r#"      http_versions = { min = "1.1", max = "1.1", unexpected = true },"#,
        ),
    ];

    for source in cases {
        let error = lua_error_from(&source);
        assert!(matches!(
            error,
            oxiroute_config_source::LuaConfigError::Lua(_)
        ));
        assert!(error.to_string().contains("unknown field `unexpected`"));
    }
}

#[test]
fn rejects_unknown_tls_and_http_version_values() {
    let cases = [
        changed("        type = \"files\",", "        type = \"pkcs12\","),
        changed(
            "      min_version = \"1.3\",",
            "      min_version = \"1.1\",",
        ),
        changed(
            "      alpn = { \"h2\", \"http/1.1\" },",
            "      alpn = { \"http/2\" },",
        ),
        with_web_pool_fields(r#"      http_versions = { min = "1", max = "1.1" },"#),
        with_web_pool_fields(r#"      http_versions = { min = "1.1", max = "4" },"#),
    ];

    for source in cases {
        assert!(matches!(
            lua_error_from(&source),
            oxiroute_config_source::LuaConfigError::Lua(_)
        ));
    }
}

#[test]
fn rejects_unknown_protocols_and_algorithms() {
    let protocol_error = lua_error_from(&changed(
        "      protocol = \"http\",",
        "      protocol = \"sctp\",",
    ));
    assert!(matches!(
        protocol_error,
        oxiroute_config_source::LuaConfigError::Lua(_)
    ));

    let algorithm_error = lua_error_from(&changed(
        r#"      endpoints = {
        { type = "socket", address = "127.0.0.1:3000" },
        { type = "socket", address = "127.0.0.1:3001" },
      },
      algorithm = "round_robin","#,
        r#"      endpoints = {
        { type = "socket", address = "127.0.0.1:3000" },
        { type = "socket", address = "127.0.0.1:3001" },
      },
      algorithm = "unknown","#,
    ));
    assert!(matches!(
        algorithm_error,
        oxiroute_config_source::LuaConfigError::Lua(_)
    ));
}

#[test]
fn does_not_expose_operating_system_functions() {
    let source = r#"
os.execute("touch /tmp/oxiroute-lua-escaped")
return { version = 1, listeners = {} }
"#;
    let error = lua_error_from(source);

    assert!(error.to_string().contains("os"));
    assert!(!std::path::Path::new("/tmp/oxiroute-lua-escaped").exists());
}

#[test]
fn enforces_the_source_size_limit() {
    let source = " ".repeat(oxiroute_config_source::MAX_SOURCE_BYTES + 1);
    let error = lua_error_from(&source);

    assert!(matches!(
        error,
        oxiroute_config_source::LuaConfigError::SourceTooLarge
    ));
}

#[test]
fn enforces_the_lua_instruction_limit() {
    let error = lua_error_from(
        r"
local counter = 0
while true do
  counter = counter + 1
end
",
    );

    assert!(matches!(
        error,
        oxiroute_config_source::LuaConfigError::Lua(_)
    ));
    assert!(error.to_string().contains("instruction limit"));
}

#[test]
fn rejects_other_forbidden_lua_globals() {
    for source in [
        r#"io.open("/tmp/oxiroute-lua-escaped", "w")
return { version = 1, listeners = {} }"#,
        r#"require("os")
return { version = 1, listeners = {} }"#,
    ] {
        let error = lua_error_from(source);
        assert!(matches!(
            error,
            oxiroute_config_source::LuaConfigError::Lua(_)
        ));
    }
}
