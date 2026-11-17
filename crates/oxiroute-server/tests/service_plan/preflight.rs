use std::fs;

use oxiroute_config::{HttpRouteAction, ListenerBind, UpstreamEndpoint};
use oxiroute_server::{ServiceKind, ServicePlanError};
use tempfile::TempDir;

use crate::canonical::canonical_config;
use crate::config_support::{loopback_endpoint as socket_endpoint, runtime_plan, service_specs};
use crate::fixture_support::{create_secure_root, write_file_with_mode};

#[cfg(unix)]
#[test]
fn runtime_preflight_preserves_every_endpoint_identity_without_connecting() {
    let mut config = canonical_config();
    config.listeners[2].bind = ListenerBind::Unix {
        path: "/tmp/oxiroute-listener-preflight-does-not-exist.sock".into(),
        mode: None,
    };
    config.upstream_pools[0].endpoints = vec![
        socket_endpoint(3000),
        UpstreamEndpoint::Dns {
            host: "not-resolved.invalid".into(),
            port: 3001,
        },
        UpstreamEndpoint::Unix {
            path: "/tmp/oxiroute-preflight-does-not-exist.sock".into(),
        },
    ];

    let validated = config.validate().unwrap();
    let services = service_specs(&validated).expect("typed endpoint services");
    assert_eq!(
        services[2].bind,
        ListenerBind::Unix {
            path: "/tmp/oxiroute-listener-preflight-does-not-exist.sock".into(),
            mode: None,
        }
    );
    let pool = services
        .iter()
        .find_map(|service| match &service.kind {
            ServiceKind::Http(service) => service.upstream_pools().next(),
            _ => None,
        })
        .expect("typed endpoint pool");
    let endpoints = &pool.health_snapshot().endpoints;
    assert_eq!(endpoints[0].address.to_string(), "127.0.0.1:3000");
    assert_eq!(
        endpoints[1].address.to_string(),
        "not-resolved.invalid:3001"
    );
    assert_eq!(
        endpoints[2].address.to_string(),
        "/tmp/oxiroute-preflight-does-not-exist.sock"
    );
}

#[cfg(unix)]
#[test]
fn http_access_and_static_preflight_is_read_only_secure_and_redacted() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let directory = TempDir::new().expect("HTTP preflight directory");
    let root = create_secure_root(directory.path(), "public");
    fs::write(root.join("index.html"), b"index").expect("static fixture");
    let token = "preflight-token-0123456789abcdef0123456789abcdef";
    let token_path = write_file_with_mode(directory.path(), "route-token", token.as_bytes(), 0o600);
    let before_entries = fs::read_dir(&root)
        .expect("root entries")
        .map(|entry| entry.expect("root entry").file_name())
        .collect::<Vec<_>>();

    let mut config = canonical_config();
    config.http_services[0].routes[0].access_policy =
        Some(oxiroute_config::HttpAccessPolicy::BearerTokenFile {
            token_file_path: token_path.clone(),
            header_name: "authorization".into(),
            realm: None,
        });
    config.http_services[0].routes[0].action = HttpRouteAction::StaticFiles {
        root_directory: root.clone(),
        path_mapping: oxiroute_config::HttpStaticPathMapping::default(),
        index_files: vec!["index.html".into()],
        internal_index_redirects: false,
        directory_redirects: false,
        spa_fallback: None,
        try_files: Vec::new(),
        autoindex: false,
        autoindex_exact_size: true,
        autoindex_local_time: false,
        etag: true,
        mime: oxiroute_config::HttpStaticMimePolicy::default(),
        headers: Vec::new(),
        error_responses: Vec::new(),
    };

    runtime_plan(&config.clone().validate().unwrap()).expect("secure read-only HTTP preflight");
    let after_entries = fs::read_dir(&root)
        .expect("root entries after preflight")
        .map(|entry| entry.expect("root entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(before_entries, after_entries);
    assert_eq!(
        fs::read_to_string(&token_path).expect("unchanged token"),
        token
    );

    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o644))
        .expect("insecure token mode");
    let Err(error) = runtime_plan(&config.clone().validate().unwrap()) else {
        panic!("insecure token mode must fail")
    };
    assert!(matches!(error, ServicePlanError::AccessPreflight { .. }));
    let diagnostic = error.to_string();
    assert!(!diagnostic.contains(token));
    assert!(!diagnostic.contains(&token_path.display().to_string()));

    let real_token = write_file_with_mode(directory.path(), "real-token", token.as_bytes(), 0o600);
    fs::remove_file(&token_path).expect("remove insecure token");
    symlink(&real_token, &token_path).expect("token symlink");
    assert!(matches!(
        runtime_plan(&config.clone().validate().unwrap()),
        Err(ServicePlanError::AccessPreflight { .. })
    ));

    config.http_services[0].routes[0].access_policy = None;
    let moved_root = directory.path().join("real-public");
    fs::rename(&root, &moved_root).expect("move static root");
    symlink(&moved_root, &root).expect("static root symlink");
    let Err(error) = runtime_plan(&config.validate().unwrap()) else {
        panic!("symlink static root must fail")
    };
    assert!(matches!(error, ServicePlanError::StaticPreflight { .. }));
    assert!(!error.to_string().contains(&root.display().to_string()));
}
