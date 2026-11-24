use std::{
    fs,
    path::{Path, PathBuf},
};

use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, LuaConfigError, MAX_SOURCE_BYTES, MAX_STRING_BYTES,
    decode_value, load_lua, resolve_source, resolve_source_with_format,
};
use tempfile::tempdir;

fn write_file(path: &Path, contents: impl AsRef<[u8]>) -> PathBuf {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directories");
    }
    fs::write(path, contents).expect("write test fixture");
    fs::canonicalize(path).expect("canonicalize written fixture")
}

#[test]
fn includes_single_and_nested_lua_modules_and_tracks_dependencies() {
    let dir = tempdir().expect("create tempdir");
    let root_path = dir.path().join("oxiroute.lua");

    let routes_path = write_file(
        &dir.path().join("routes.lua"),
        r#"
return {
  {
    host = { kind = "normalized_host", value = "api.example.com" },
    path = { kind = "segment_prefix", value = "/v1" },
    methods = { "GET" },
    action = {
      type = "proxy",
      upstream_pool = "api-backends",
      policy = {},
    },
  },
}
"#,
    );

    let nested_b_path = write_file(
        &dir.path().join("sub/b.lua"),
        r#"
return {
  { type = "socket", address = "127.0.0.1:4001" },
  { type = "socket", address = "127.0.0.1:4002" },
}
"#,
    );

    let nested_a_path = write_file(
        &dir.path().join("sub/a.lua"),
        r#"
local endpoints = include("b.lua")
return {
  {
    name = "api-backends",
    endpoints = endpoints,
    algorithm = "round_robin",
  },
}
"#,
    );

    let root_source = r#"
local routes = include("routes.lua")
local pools = include("sub/a.lua")
return {
  version = 1,
  listeners = {
    {
      name = "web",
      bind = { type = "socket", address = "127.0.0.1:8080" },
      protocol = "http",
      service = "web",
    },
  },
  upstream_pools = pools,
  http_services = {
    {
      name = "web",
      routes = routes,
    },
  },
}
"#;

    let resolved = resolve_source(&root_path, root_source.as_bytes()).expect("resolve lua config");
    assert_eq!(resolved.format, ConfigFormat::Lua);
    assert!(resolved.compositional);
    let draft = resolved.config.as_draft();
    assert_eq!(draft.upstream_pools.len(), 1);
    assert_eq!(draft.upstream_pools[0].servers.len(), 2);
    assert_eq!(draft.http_services.len(), 1);
    assert_eq!(draft.http_services[0].routes.len(), 1);

    assert!(resolved.dependencies.contains(&routes_path));
    assert!(resolved.dependencies.contains(&nested_a_path));
    assert!(resolved.dependencies.contains(&nested_b_path));
}

fn write_external_data_fixtures(root: &Path) -> [PathBuf; 5] {
    [
        write_file(
            &root.join("data/cert_chain_path.txt"),
            "/etc/oxiroute/web-chain.pem",
        ),
        write_file(
            &root.join("data/domains.txt"),
            "# Allowed DNS names\n\n  example.com  \n# comment\n  www.example.com\n\n",
        ),
        write_file(
            &root.join("data/pools.json"),
            r#"{
  "pools": [
    {
      "name": "web-backends",
      "endpoints": [
        { "type": "socket", "address": "127.0.0.1:3000" }
      ],
      "algorithm": "round_robin"
    }
  ]
}"#,
        ),
        write_file(
            &root.join("data/tls.yaml"),
            r#"
name: web-tls
certificates:
  - web-cert
default_certificate: web-cert
min_version: "1.3"
alpn:
  - h2
  - http/1.1
"#,
        ),
        write_file(
            &root.join("data/listener.toml"),
            r#"
name = "web"
protocol = "http"
service = "web"
tls_profile = "web-tls"
max_connections = 2048

[bind]
type = "socket"
address = "127.0.0.1:8443"
"#,
        ),
    ]
}

#[test]
fn reads_external_text_lines_json_yaml_and_toml_data_files() {
    let dir = tempdir().expect("create tempdir");
    let root_path = dir.path().join("oxiroute.lua");
    let expected_deps = write_external_data_fixtures(dir.path());

    let root_source = r#"
local chain_path = read_text("data/cert_chain_path.txt")
local domains = read_lines("data/domains.txt")
local pools_doc = read_json("data/pools.json")
local tls_profile = read_yaml("data/tls.yaml")
local listener = read_toml("data/listener.toml")

return {
  version = 1,
  certificates = {
    {
      name = "web-cert",
      dns_names = domains,
      source = {
        type = "files",
        certificate_chain_path = chain_path,
        private_key_path = "/etc/oxiroute/web-key.pem",
      },
    },
  },
  tls_profiles = { tls_profile },
  listeners = { listener },
  upstream_pools = pools_doc.pools,
  http_services = {
    {
      name = "web",
      routes = {
        {
          host = { kind = "normalized_host", value = domains[1] },
          path = { kind = "segment_prefix", value = "/" },
          action = {
            type = "proxy",
            upstream_pool = "web-backends",
            policy = {},
          },
        },
      },
    },
  },
}
"#;

    let resolved =
        resolve_source_with_format(&root_path, root_source.as_bytes(), ConfigFormat::Lua)
            .expect("resolve lua with external readers");
    assert!(resolved.compositional);
    let draft = resolved.config.as_draft();
    assert_eq!(draft.certificates.len(), 1);
    assert_eq!(draft.certificates[0].dns_names.len(), 2);
    assert_eq!(draft.tls_profiles.len(), 1);
    assert_eq!(draft.listeners.len(), 1);
    assert_eq!(draft.upstream_pools.len(), 1);
    assert_eq!(draft.upstream_pools[0].servers.len(), 1);

    for expected in &expected_deps {
        assert!(
            resolved.dependencies.contains(expected),
            "expected dependency {} to be tracked",
            expected.display()
        );
    }
}

#[test]
fn rejects_parent_traversal_and_absolute_paths() {
    let dir = tempdir().expect("create tempdir");
    let nested_dir = dir.path().join("cfg");
    fs::create_dir_all(&nested_dir).expect("create cfg dir");
    write_file(&dir.path().join("escape.lua"), "return { version = 1 }");

    let root_path = nested_dir.join("oxiroute.lua");

    let traversal_err = resolve_source(&root_path, b"return include('../escape.lua')").unwrap_err();
    assert!(matches!(traversal_err, ConfigSourceError::Lua(_)));
    assert!(
        traversal_err.to_string().contains(".."),
        "unexpected error: {traversal_err}"
    );

    let absolute_err = resolve_source(&root_path, b"return read_text('/etc/passwd')").unwrap_err();
    assert!(matches!(absolute_err, ConfigSourceError::Lua(_)));
    assert!(
        absolute_err.to_string().contains("relative"),
        "unexpected error: {absolute_err}"
    );
}

#[cfg(unix)]
#[test]
fn rejects_file_and_directory_symlinks() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().expect("create tempdir");
    let outside = tempdir().expect("create outside dir");
    let outside_file = write_file(&outside.path().join("secret.lua"), "return { version = 1 }");
    let outside_dir = outside.path().join("nested");
    write_file(&outside_dir.join("secret.txt"), "secret");

    let file_link = dir.path().join("link.lua");
    symlink(&outside_file, &file_link).expect("create file symlink");

    let dir_link = dir.path().join("linked_dir");
    symlink(&outside_dir, &dir_link).expect("create dir symlink");

    let root_path = dir.path().join("oxiroute.lua");

    let file_link_err = resolve_source(&root_path, b"return include('link.lua')").unwrap_err();
    assert!(matches!(file_link_err, ConfigSourceError::Lua(_)));
    assert!(
        file_link_err.to_string().contains("symlink"),
        "unexpected error: {file_link_err}"
    );

    let dir_link_err =
        resolve_source(&root_path, b"return read_text('linked_dir/secret.txt')").unwrap_err();
    assert!(matches!(dir_link_err, ConfigSourceError::Lua(_)));
    assert!(
        dir_link_err.to_string().contains("symlink"),
        "unexpected error: {dir_link_err}"
    );
}

#[test]
fn rejects_include_cycles() {
    let dir = tempdir().expect("create tempdir");
    write_file(&dir.path().join("a.lua"), "return include('b.lua')");
    write_file(&dir.path().join("b.lua"), "return include('a.lua')");

    let root_path = dir.path().join("oxiroute.lua");
    let err = resolve_source(&root_path, b"return include('a.lua')").unwrap_err();
    assert!(matches!(err, ConfigSourceError::Lua(_)));
    assert!(err.to_string().contains("cycle"), "unexpected error: {err}");
}

#[test]
fn rejects_include_depth_exceeded() {
    let dir = tempdir().expect("create tempdir");
    for level in 0..17 {
        let body = if level < 16 {
            format!("return include('level_{}.lua')", level + 1)
        } else {
            "return { version = 1 }".to_owned()
        };
        write_file(&dir.path().join(format!("level_{level}.lua")), body);
    }

    let root_path = dir.path().join("oxiroute.lua");
    let err = resolve_source(&root_path, b"return include('level_0.lua')").unwrap_err();
    assert!(matches!(err, ConfigSourceError::Lua(_)));
    assert!(err.to_string().contains("depth"), "unexpected error: {err}");
}

#[test]
fn rejects_missing_invalid_utf8_and_oversized_files() {
    let dir = tempdir().expect("create tempdir");
    let root_path = dir.path().join("oxiroute.lua");

    let missing_err = resolve_source(&root_path, b"return include('missing.lua')").unwrap_err();
    assert!(matches!(missing_err, ConfigSourceError::Lua(_)));
    assert!(
        missing_err.to_string().contains("not found"),
        "unexpected error: {missing_err}"
    );

    write_file(&dir.path().join("bad.txt"), [0xFF, 0xFE, 0xFD]);
    let utf8_err = resolve_source(&root_path, b"return read_text('bad.txt')").unwrap_err();
    assert!(matches!(utf8_err, ConfigSourceError::Lua(_)));
    assert!(
        utf8_err.to_string().contains("UTF-8"),
        "unexpected error: {utf8_err}"
    );

    write_file(
        &dir.path().join("huge.txt"),
        "a".repeat(MAX_STRING_BYTES + 1),
    );
    let oversized_text_err =
        resolve_source(&root_path, b"return read_text('huge.txt')").unwrap_err();
    assert!(matches!(oversized_text_err, ConfigSourceError::Lua(_)));
    assert!(
        oversized_text_err.to_string().contains("size limit"),
        "unexpected error: {oversized_text_err}"
    );

    write_file(
        &dir.path().join("huge.lua"),
        " ".repeat(MAX_SOURCE_BYTES + 1),
    );
    let oversized_lua_err = resolve_source(&root_path, b"return include('huge.lua')").unwrap_err();
    assert!(matches!(oversized_lua_err, ConfigSourceError::Lua(_)));
    assert!(
        oversized_lua_err.to_string().contains("size limit"),
        "unexpected error: {oversized_lua_err}"
    );
}

#[test]
fn memory_only_evaluation_fails_closed_for_all_loader_functions() {
    for expr in [
        "return include('routes.lua')",
        "return read_text('data.txt')",
        "return read_lines('domains.txt')",
        "return read_json('pools.json')",
        "return read_yaml('tls.yaml')",
        "return read_toml('listener.toml')",
    ] {
        let load_err = load_lua(expr).unwrap_err();
        assert!(matches!(load_err, LuaConfigError::Lua(_)));
        assert!(
            load_err
                .to_string()
                .contains("filesystem loading is disabled in memory-only Lua evaluation"),
            "unexpected load_lua error for `{expr}`: {load_err}"
        );

        let decode_err = decode_value(ConfigFormat::Lua, expr.as_bytes()).unwrap_err();
        assert!(matches!(decode_err, ConfigSourceError::Lua(_)));
        assert!(
            decode_err
                .to_string()
                .contains("filesystem loading is disabled in memory-only Lua evaluation"),
            "unexpected decode_value error for `{expr}`: {decode_err}"
        );
    }
}
