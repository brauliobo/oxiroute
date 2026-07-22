use oxiroute_config::{load_lua, Protocol};

const VALID_CONFIG: &str = r#"
return {
  version = 1,
  listeners = {
    {
      name = "web",
      bind = "127.0.0.1:8080",
      protocol = "http",
      upstream = "127.0.0.1:3000",
    },
    {
      name = "database",
      bind = "127.0.0.1:5432",
      protocol = "tcp",
      upstream = "10.0.0.12:5432",
    },
  },
}
"#;

#[test]
fn loads_a_minimal_lua_configuration() {
    let config = load_lua(VALID_CONFIG).expect("valid configuration");

    assert_eq!(config.version, 1);
    assert_eq!(config.listeners.len(), 2);
    assert_eq!(config.listeners[0].name, "web");
    assert_eq!(config.listeners[0].protocol, Protocol::Http);
    assert_eq!(config.listeners[0].bind.to_string(), "127.0.0.1:8080");
    assert_eq!(config.listeners[0].upstream.to_string(), "127.0.0.1:3000");
    assert_eq!(config.listeners[1].protocol, Protocol::Tcp);
}

#[test]
fn rejects_unknown_fields() {
    let source = VALID_CONFIG.replace("version = 1,", "version = 1,\n  typo = true,");
    let error = load_lua(&source).expect_err("unknown fields must not be ignored");

    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn rejects_duplicate_listener_binds() {
    let source = VALID_CONFIG.replace("127.0.0.1:5432", "127.0.0.1:8080");
    let error = load_lua(&source).expect_err("binds must be unique");

    assert!(error.to_string().contains("duplicate listener bind"));
}

#[test]
fn does_not_expose_operating_system_functions() {
    let source = r#"
os.execute("touch /tmp/oxiroute-lua-escaped")
return { version = 1, listeners = {} }
"#;
    let error = load_lua(source).expect_err("the Lua environment must be restricted");

    assert!(error.to_string().contains("os"));
    assert!(!std::path::Path::new("/tmp/oxiroute-lua-escaped").exists());
}
