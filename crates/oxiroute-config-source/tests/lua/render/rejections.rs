use std::path::PathBuf;

use crate::fixtures::{
    MAX_SAFE_INTEGER, complete_config, minimal_config, normalized_round_trip, socket,
    test_rtmp_services,
};
use crate::lua_support::{load_lua, render_lua, render_lua_error};
use crate::mutations::Mutation;
use oxiroute_config::{
    ConfigDraft, ConfigError, DownstreamTimeoutPolicy, HttpAccessPolicy, HttpRouteAction, Listener,
    ListenerBind, Management, Protocol, UpstreamEndpoint,
};

#[test]
fn escapes_strings_and_paths_without_changing_values() {
    let mut config = complete_config();
    config.listeners[0].name = "edge \"quoted\" \\ slash café".into();
    config.management.as_mut().expect("management").ui_dir =
        Some(PathBuf::from("/tmp/line\n\"quote\"\\tail"));

    let (normalized, source) = normalized_round_trip(config);

    assert!(source.contains(r#"name = "edge \"quoted\" \\ slash café","#));
    assert!(source.contains(r#"ui_dir = "/tmp/line\n\"quote\"\\tail","#));
    assert_eq!(load_lua(&source).expect("escaped reload"), normalized);
}

#[cfg(unix)]
#[test]
fn rejects_non_utf8_paths_without_lossy_exposure() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    let assert_render_error = |config: &ConfigDraft, expected: &str| {
        let message = render_lua(config)
            .expect_err("expected a Lua rendering error")
            .to_string();
        assert!(message.contains(expected), "unexpected error: {message}");
        assert!(!message.contains('\u{fffd}'));
    };

    let mut config = minimal_config();
    config.management = Some(Management {
        bind: socket("127.0.0.1:9080"),
        ui_dir: Some(PathBuf::from(OsString::from_vec(
            b"/tmp/secret-\xff".to_vec(),
        ))),
        token_file: None,
    });

    assert_render_error(
        &config,
        "management `management` has invalid `ui_dir`: path must be valid UTF-8",
    );
    assert!(serde_json::to_string(&config).is_err());

    let non_utf8 = PathBuf::from(OsString::from_vec(b"/run/oxiroute/socket-\xff".to_vec()));
    let mut listener = complete_config();
    listener.listeners[3].bind = ListenerBind::Unix {
        path: non_utf8.clone(),
        mode: None,
    };
    assert_render_error(
        &listener,
        "listener `live` has invalid Unix socket `bind.path`: path must be valid UTF-8",
    );

    let mut endpoint = complete_config();
    endpoint.upstream_pools[3].servers[0].endpoint = UpstreamEndpoint::Unix { path: non_utf8 };
    assert_render_error(
        &endpoint,
        "upstream pool `unix-backends` has invalid Unix socket `endpoints[].path`: path must be valid UTF-8",
    );

    let mut recorder = complete_config();
    recorder.rtmp_services[0].applications[0].recorders[0].root_directory = PathBuf::from(
        OsString::from_vec(b"/var/lib/oxiroute/recordings-\xff".to_vec()),
    );
    assert_render_error(
        &recorder,
        "RTMP recorder `continuous` in application `live` of service `live` has invalid `root_directory`: path must be valid UTF-8",
    );

    let mut access = complete_config();
    let Some(HttpAccessPolicy::BearerTokenFile {
        token_file_path, ..
    }) = &mut access.http_services[0].routes[0].access_policy
    else {
        panic!("bearer access policy");
    };
    *token_file_path = PathBuf::from(OsString::from_vec(b"/run/secrets/token-\xff".to_vec()));
    assert_render_error(
        &access,
        "HTTP access policy `web route 0` has invalid `token_file_path`: path must be valid UTF-8",
    );

    let mut static_files = complete_config();
    let HttpRouteAction::StaticFiles { root_directory, .. } =
        &mut static_files.http_services[0].routes[3].action
    else {
        panic!("static-files action");
    };
    *root_directory = PathBuf::from(OsString::from_vec(b"/srv/www-\xff".to_vec()));
    assert_render_error(
        &static_files,
        "HTTP service `web` route 3 has invalid `action.static_files.root_directory`: path must be valid UTF-8",
    );
}

#[test]
fn rejects_u64_values_that_json_and_lua_cannot_round_trip_exactly() {
    let mutations: &[Mutation] = &[
        ("upstream_io_timeout_ms", |config| {
            config.http_services[0].upstream_io_timeout_ms = MAX_SAFE_INTEGER + 1;
        }),
        ("max_request_body_bytes", |config| {
            config.http_services[0].max_request_body_bytes = Some(MAX_SAFE_INTEGER + 1);
        }),
        ("connect_timeout_ms", |config| {
            config.l4_services[0].connect_timeout_ms = MAX_SAFE_INTEGER + 1;
        }),
        ("idle_timeout_ms", |config| {
            config.l4_services[0].idle_timeout_ms = MAX_SAFE_INTEGER + 1;
        }),
        ("lifetime_timeout_ms", |config| {
            config.l4_services[0].lifetime_timeout_ms = Some(MAX_SAFE_INTEGER + 1);
        }),
    ];

    for (expected_field, mutate) in mutations {
        let mut config = complete_config();
        mutate(&mut config);
        assert!(matches!(
            render_lua(&config),
            Err(ConfigError::LimitTooLarge { field, .. }) if field == *expected_field
        ));
    }
}

#[test]
fn rejects_rendered_sources_beyond_the_loader_limit() {
    let mut config = minimal_config();
    config.listeners.push(Listener {
        name: "x".repeat(oxiroute_config_source::MAX_SOURCE_BYTES),
        bind: ListenerBind::Socket {
            address: socket("127.0.0.1:1935"),
            ipv6_only: false,
        },
        protocol: Protocol::Rtmp,
        service: Some("live".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: Some(1),
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
        h2_initial_stream_window: None,
        h2_initial_connection_window: None,
    });
    config.rtmp_services = test_rtmp_services();

    assert!(matches!(
        render_lua_error(&config),
        oxiroute_config_source::LuaConfigError::SourceTooLarge
    ));
}
