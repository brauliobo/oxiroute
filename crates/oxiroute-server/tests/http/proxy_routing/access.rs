use crate::TEST_TIMEOUT;
use crate::fixture_support::create_secure_root;
use crate::fixture_support::write_secure_token;
use crate::support::harness::ProxyHarness;
use oxiroute_config::HttpAccessPolicy;
use oxiroute_config::HttpPathSelector;
use oxiroute_config::HttpRoute;
use oxiroute_config::HttpRouteAction;
use oxiroute_config::HttpStaticErrorResponse;
use oxiroute_config::HttpStaticMimePolicy;
use oxiroute_config::HttpStaticPathMapping;
use oxiroute_config::HttpStaticTryFile;
use std::fs;
use tokio::time::timeout;

#[tokio::test]
async fn bearer_access_uses_the_configured_header_without_exposing_the_token() {
    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("access token directory");
        let token = "route-token-0123456789abcdef0123456789abcdef";
        let token_path = write_secure_token(directory.path(), "route-token", token);
        let route = HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: Some(HttpAccessPolicy::BearerTokenFile {
                token_file_path: token_path.clone(),
                header_name: "x-route-token".into(),
                realm: Some("private".into()),
            }),
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::FixedResponse {
                status: 200,
                body: "authorized".into(),
                headers: Vec::new(),
            },
        };
        let proxy = ProxyHarness::start(Vec::new(), vec![route], 1024, 3).await;

        let missing = proxy
            .request("GET / HTTP/1.1\r\nHost: access.test\r\n")
            .await;
        assert_eq!(missing.status, 401);
        assert_eq!(
            missing.header("www-authenticate"),
            Some("Bearer realm=\"private\"")
        );
        let wrong = proxy
            .request("GET / HTTP/1.1\r\nHost: access.test\r\nX-Route-Token: Bearer wrong\r\n")
            .await;
        assert_eq!(wrong.status, 401);
        let accepted = proxy
            .request(&format!(
                "GET / HTTP/1.1\r\nHost: access.test\r\nX-Route-Token: Bearer {token}\r\n"
            ))
            .await;
        assert_eq!(accepted.status, 200);
        assert_eq!(accepted.body(), b"authorized");
        assert!(!accepted.text().contains(token));
        assert!(!accepted.text().contains(&token_path.display().to_string()));

        proxy.finish().await;
    })
    .await
    .expect("route access test timed out");
}

#[tokio::test]
async fn basic_access_verifies_apr1_for_known_and_unknown_users_without_exposing_credentials() {
    use std::os::unix::fs::PermissionsExt as _;

    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("htpasswd directory");
        let path = directory.path().join("users.htpasswd");
        fs::write(
            &path,
            concat!(
                "first:$apr1$r31.....$HqJZimcKQFAMYayBlzkrA/\n",
                "myName:$apr1$r31.....$HqJZimcKQFAMYayBlzkrA/\n",
                "last:$apr1$r31.....$HqJZimcKQFAMYayBlzkrA/\n",
            ),
        )
        .expect("write htpasswd");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("secure htpasswd mode");
        let route = HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: Some(HttpAccessPolicy::BasicHtpasswdFile {
                htpasswd_file_path: path.clone(),
                realm: "private".into(),
            }),
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::FixedResponse {
                status: 200,
                body: "authorized".into(),
                headers: Vec::new(),
            },
        };
        let proxy = ProxyHarness::start(Vec::new(), vec![route], 1024, 6).await;

        let missing = proxy
            .request("GET / HTTP/1.1\r\nHost: basic.test\r\n")
            .await;
        assert_eq!(
            missing.header("www-authenticate"),
            Some("Basic realm=\"private\", charset=\"UTF-8\"")
        );
        let wrong = proxy
            .request("GET / HTTP/1.1\r\nHost: basic.test\r\nAuthorization: Basic bXlOYW1lOndyb25n\r\n")
            .await;
        assert_eq!(wrong.status, 401);
        let unknown = proxy
            .request("GET / HTTP/1.1\r\nHost: basic.test\r\nAuthorization: Basic dW5rbm93bjp3cm9uZw==\r\n")
            .await;
        assert_eq!(unknown.status, 401);
        let accepted = proxy
            .request("GET / HTTP/1.1\r\nHost: basic.test\r\nAuthorization: bAsIc bXlOYW1lOm15UGFzc3dvcmQ=\r\n")
            .await;
        assert_eq!(accepted.status, 200);
        assert_eq!(accepted.body(), b"authorized");
        let first = proxy
            .request("GET / HTTP/1.1\r\nHost: basic.test\r\nAuthorization: Basic Zmlyc3Q6bXlQYXNzd29yZA==\r\n")
            .await;
        assert_eq!(first.status, 200);
        let last = proxy
            .request("GET / HTTP/1.1\r\nHost: basic.test\r\nAuthorization: Basic bGFzdDpteVBhc3N3b3Jk\r\n")
            .await;
        assert_eq!(last.status, 200);
        assert!(!accepted.text().contains("bXlOYW1l"));
        assert!(!accepted.text().contains(&path.display().to_string()));

        proxy.finish().await;
    })
    .await
    .expect("Basic htpasswd test timed out");
}

#[tokio::test]
async fn basic_access_preserves_bcrypt_verification() {
    use std::os::unix::fs::PermissionsExt as _;

    use base64::{Engine as _, engine::general_purpose::STANDARD};

    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("htpasswd directory");
        let path = directory.path().join("users.htpasswd");
        let first_hash = bcrypt::hash_with_salt(b"firstPassword", 5, *b"FirstUserSalt123")
            .expect("first bcrypt fixture");
        let last_hash = bcrypt::hash_with_salt(b"lastPassword", 5, *b"LastUserSalt_123")
            .expect("last bcrypt fixture");
        fs::write(
            &path,
            format!(
                concat!(
                    "first:{}\n",
                    "myName:$2y$05$c4WoMPo3SXsafkva.HHa6uXQZWr7oboPiC2bT/r7q1BB8I2s0BRqC\n",
                    "last:{}\n",
                ),
                first_hash, last_hash,
            ),
        )
        .expect("write htpasswd");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("secure htpasswd mode");
        let route = HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: Some(HttpAccessPolicy::BasicHtpasswdFile {
                htpasswd_file_path: path,
                realm: "private".into(),
            }),
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::FixedResponse {
                status: 200,
                body: "authorized".into(),
                headers: Vec::new(),
            },
        };
        let proxy = ProxyHarness::start(Vec::new(), vec![route], 1024, 4).await;

        let wrong = proxy
            .request("GET / HTTP/1.1\r\nHost: basic.test\r\nAuthorization: Basic bXlOYW1lOndyb25n\r\n")
            .await;
        assert_eq!(wrong.status, 401);
        let accepted = proxy
            .request("GET / HTTP/1.1\r\nHost: basic.test\r\nAuthorization: Basic bXlOYW1lOm15UGFzc3dvcmQ=\r\n")
            .await;
        assert_eq!(accepted.status, 200);
        let first = proxy
            .request(&format!(
                "GET / HTTP/1.1\r\nHost: basic.test\r\nAuthorization: Basic {}\r\n",
                STANDARD.encode("first:firstPassword")
            ))
            .await;
        assert_eq!(first.status, 200);
        let last = proxy
            .request(&format!(
                "GET / HTTP/1.1\r\nHost: basic.test\r\nAuthorization: Basic {}\r\n",
                STANDARD.encode("last:lastPassword")
            ))
            .await;
        assert_eq!(last.status, 200);

        proxy.finish().await;
    })
    .await
    .expect("bcrypt regression test timed out");
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one adversarial scenario covers index, error, access, and loop rerouting"
)]
async fn nginx_internal_static_redirects_reselect_exact_routes_and_recheck_basic_access() {
    use std::os::unix::fs::PermissionsExt as _;

    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("internal redirect fixture");
        let root = create_secure_root(directory.path(), "public");
        fs::write(root.join("private.html"), b"private index").expect("private index");
        fs::write(root.join("50x.html"), b"private error").expect("private error");
        let htpasswd = directory.path().join("users.htpasswd");
        fs::write(
            &htpasswd,
            b"myName:$2y$05$c4WoMPo3SXsafkva.HHa6uXQZWr7oboPiC2bT/r7q1BB8I2s0BRqC\n",
        )
        .expect("write htpasswd");
        fs::set_permissions(&htpasswd, fs::Permissions::from_mode(0o600))
            .expect("secure htpasswd mode");
        let access = Some(HttpAccessPolicy::BasicHtpasswdFile {
            htpasswd_file_path: htpasswd,
            realm: "private".into(),
        });
        let static_action = |indexes: Vec<String>, try_files, error_responses| {
            HttpRouteAction::StaticFiles {
                root_directory: root.clone(),
                path_mapping: HttpStaticPathMapping::Root,
                index_files: indexes,
                internal_index_redirects: true,
                directory_redirects: true,
                spa_fallback: None,
                try_files,
                autoindex: false,
                autoindex_exact_size: true,
                autoindex_local_time: false,
                etag: true,
                mime: HttpStaticMimePolicy {
                    default_type: Some("text/plain".into()),
                    types: Vec::new(),
                },
                headers: Vec::new(),
                error_responses,
            }
        };
        let routes = vec![
            HttpRoute {
                host: None,
                path: HttpPathSelector::SegmentPrefix { value: "/".into() },
                methods: Vec::new(),
                access_policy: None,
                policy: oxiroute_config::HttpRoutePolicy::default(),
                action: static_action(vec!["private.html".into()], Vec::new(), Vec::new()),
            },
            HttpRoute {
                host: None,
                path: HttpPathSelector::Exact {
                    value: "/private.html".into(),
                },
                methods: Vec::new(),
                access_policy: access.clone(),
                policy: oxiroute_config::HttpRoutePolicy::default(),
                action: static_action(Vec::new(), Vec::new(), Vec::new()),
            },
            HttpRoute {
                host: None,
                path: HttpPathSelector::Exact {
                    value: "/error".into(),
                },
                methods: Vec::new(),
                access_policy: None,
                policy: oxiroute_config::HttpRoutePolicy::default(),
                action: static_action(
                    Vec::new(),
                    vec![HttpStaticTryFile::Status { status: 503 }],
                    vec![HttpStaticErrorResponse {
                        statuses: vec![503],
                        file: Some("50x.html".into()),
                        body: None,
                        headers: Vec::new(),
                        internal_redirect: Some("/50x.html".into()),
                    }],
                ),
            },
            HttpRoute {
                host: None,
                path: HttpPathSelector::Exact {
                    value: "/50x.html".into(),
                },
                methods: Vec::new(),
                access_policy: access,
                policy: oxiroute_config::HttpRoutePolicy::default(),
                action: static_action(Vec::new(), Vec::new(), Vec::new()),
            },
            HttpRoute {
                host: None,
                path: HttpPathSelector::Exact {
                    value: "/loop".into(),
                },
                methods: Vec::new(),
                access_policy: None,
                policy: oxiroute_config::HttpRoutePolicy::default(),
                action: static_action(
                    Vec::new(),
                    vec![HttpStaticTryFile::Status { status: 503 }],
                    vec![HttpStaticErrorResponse {
                        statuses: vec![503],
                        file: Some("loop".into()),
                        body: None,
                        headers: Vec::new(),
                        internal_redirect: Some("/loop".into()),
                    }],
                ),
            },
        ];
        let proxy = ProxyHarness::start(Vec::new(), routes, 1024, 5).await;

        let index_denied = proxy.request("GET / HTTP/1.1\r\nHost: static.test\r\n").await;
        assert_eq!(index_denied.status, 401);
        let index = proxy
            .request("GET / HTTP/1.1\r\nHost: static.test\r\nAuthorization: Basic bXlOYW1lOm15UGFzc3dvcmQ=\r\n")
            .await;
        assert_eq!(index.status, 200);
        assert_eq!(index.body(), b"private index");

        let error_denied = proxy
            .request("GET /error HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(error_denied.status, 401);
        let error = proxy
            .request("GET /error HTTP/1.1\r\nHost: static.test\r\nAuthorization: Basic bXlOYW1lOm15UGFzc3dvcmQ=\r\n")
            .await;
        assert_eq!(error.status, 503);
        assert_eq!(error.body(), b"private error");

        let looped = proxy
            .request("GET /loop HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(looped.status, 500);

        proxy.finish().await;
    })
    .await
    .expect("internal redirect policy test timed out");
}
