use crate::TEST_TIMEOUT;
use crate::fixture_support::create_secure_root;
use crate::support::harness::ProxyHarness;
use oxiroute_config::HttpLiteralHeader;
use oxiroute_config::HttpMimeType;
use oxiroute_config::HttpPathSelector;
use oxiroute_config::HttpRoute;
use oxiroute_config::HttpRouteAction;
use oxiroute_config::HttpStaticErrorResponse;
use oxiroute_config::HttpStaticMimePolicy;
use oxiroute_config::HttpStaticPathMapping;
use oxiroute_config::HttpStaticTryFile;
use std::fs;
use std::os::unix::fs::symlink;
use tokio::time::timeout;

#[tokio::test]
async fn static_files_pin_the_root_and_reject_symlinks_while_supporting_indexes_and_spa() {
    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("static fixture directory");
        let root = create_secure_root(directory.path(), "public");
        fs::write(root.join("index.html"), b"index").expect("write index");
        fs::write(root.join("app.js"), b"javascript").expect("write asset");
        fs::write(root.join("spa.html"), b"fallback").expect("write fallback");
        symlink("/etc/passwd", root.join("escape.txt")).expect("write escape symlink");
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            root.join("special"),
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .expect("write special-file fixture");
        let oversized = fs::File::create(root.join("oversized.bin")).expect("oversized fixture");
        oversized
            .set_len(8 * 1024 * 1024 * 1024 + 1)
            .expect("oversized fixture length");
        let route = HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::StaticFiles {
                root_directory: root.clone(),
                path_mapping: oxiroute_config::HttpStaticPathMapping::default(),
                index_files: vec!["index.html".into()],
                internal_index_redirects: false,
                directory_redirects: false,
                spa_fallback: Some("spa.html".into()),
                try_files: Vec::new(),
                autoindex: false,
                autoindex_exact_size: true,
                autoindex_local_time: false,
                etag: true,
                mime: oxiroute_config::HttpStaticMimePolicy::default(),
                headers: Vec::new(),
                error_responses: Vec::new(),
            },
        };
        let proxy = ProxyHarness::start(Vec::new(), vec![route], 1024, 8).await;

        let moved = directory.path().join("pinned-public");
        fs::rename(&root, &moved).expect("move pinned root");
        fs::create_dir(&root).expect("replace configured root path");
        fs::write(root.join("index.html"), b"replacement").expect("write replacement root");

        let index = proxy
            .request("GET / HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(index.status, 200);
        assert_eq!(index.body(), b"index");
        assert_eq!(
            index.header("content-type"),
            Some("text/html; charset=utf-8")
        );

        let head = proxy
            .request("HEAD /app.js HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(head.status, 200);
        assert_eq!(head.header("content-length"), Some("10"));
        assert_eq!(
            head.header("content-type"),
            Some("text/javascript; charset=utf-8")
        );
        assert!(head.body().is_empty());

        let fallback = proxy
            .request("GET /client/route HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(fallback.status, 200);
        assert_eq!(fallback.body(), b"fallback");

        let symlink_response = proxy
            .request("GET /escape.txt HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(symlink_response.status, 403);
        assert!(!symlink_response.text().contains("root:"));

        let special = proxy
            .request("GET /special HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(special.status, 403);

        let oversized = proxy
            .request("GET /oversized.bin HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(oversized.status, 500);

        let method = proxy
            .request("POST /app.js HTTP/1.1\r\nHost: static.test\r\nContent-Length: 0\r\n")
            .await;
        assert_eq!(method.status, 405);
        assert_eq!(method.header("allow"), Some("GET, HEAD"));

        let traversal = proxy
            .request("GET /../etc/passwd HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(traversal.status, 400);

        proxy.finish().await;
    })
    .await
    .expect("static action test timed out");
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one wire scenario covers the complete imported static policy"
)]
async fn host_shaped_static_policy_covers_root_alias_try_files_index_autoindex_mime_errors_and_ranges()
 {
    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("extended static fixture");
        let root = create_secure_root(directory.path(), "public");
        fs::write(root.join("asset.custom"), b"custom asset").expect("write custom asset");
        fs::write(root.join("fallback.txt"), b"fallback").expect("write fallback");
        fs::write(root.join("50x.html"), b"custom 50x").expect("write error document");
        fs::create_dir(root.join("directory")).expect("write index directory");
        fs::write(root.join("directory/index.html"), b"directory index")
            .expect("write directory index");
        fs::create_dir(root.join("empty-directory")).expect("write empty directory");
        fs::create_dir(root.join("root")).expect("write root mapping directory");
        fs::write(root.join("root/root.txt"), b"root mapped").expect("write root mapped file");
        fs::write(root.join("listing-entry.bin"), vec![b'x'; 1536])
            .expect("write listing entry");
        let large = fs::File::create(root.join("large.bin")).expect("write large fixture");
        large.set_len(17 * 1024 * 1024).expect("large fixture size");

        let proxy = ProxyHarness::start(
            Vec::new(),
            host_shaped_static_routes(&root),
            1024,
            25,
        )
        .await;

        let asset = proxy
            .request("GET /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(asset.status, 200);
        assert_eq!(asset.header("content-type"), Some("application/x-custom"));
        assert_eq!(asset.header("x-static-policy"), Some("host-shaped"));
        assert_eq!(asset.body(), b"custom asset");
        let etag = asset.header("etag").expect("static ETag").to_owned();
        let last_modified = asset
            .header("last-modified")
            .expect("static Last-Modified")
            .to_owned();

        let etag_source = proxy
            .request("GET /assets/fallback.txt HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        let shared_etag = etag_source
            .header("etag")
            .expect("enabled ETag for shared static file")
            .to_owned();
        let shared_last_modified = etag_source
            .header("last-modified")
            .expect("Last-Modified for shared static file")
            .to_owned();
        let etag_disabled = proxy
            .request(&format!(
                "GET /fallback/fallback.txt HTTP/1.1\r\nHost: static.test\r\nIf-None-Match: {shared_etag}\r\n"
            ))
            .await;
        assert_eq!(etag_disabled.status, 200);
        assert_eq!(etag_disabled.body(), b"fallback");
        assert!(etag_disabled.header("etag").is_none());
        assert!(etag_disabled.header("last-modified").is_some());
        assert_eq!(
            etag_disabled.header("x-static-policy"),
            Some("host-shaped")
        );
        let etag_disabled_precedence = proxy
            .request(&format!(
                "GET /fallback/fallback.txt HTTP/1.1\r\nHost: static.test\r\nIf-None-Match: {shared_etag}\r\nIf-Modified-Since: {shared_last_modified}\r\n"
            ))
            .await;
        assert_eq!(etag_disabled_precedence.status, 200);
        assert_eq!(etag_disabled_precedence.body(), b"fallback");
        let etag_disabled_wildcard = proxy
            .request("GET /fallback/fallback.txt HTTP/1.1\r\nHost: static.test\r\nIf-None-Match: *\r\n")
            .await;
        assert_eq!(etag_disabled_wildcard.status, 304);
        assert!(etag_disabled_wildcard.header("etag").is_none());

        let failed_match = proxy
            .request("GET /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nIf-Match: \"stale\"\r\n")
            .await;
        assert_eq!(failed_match.status, 412);
        assert_eq!(failed_match.header("etag"), Some(etag.as_str()));

        let not_modified_since = proxy
            .request(&format!(
                "GET /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nIf-Modified-Since: {last_modified}\r\n"
            ))
            .await;
        assert_eq!(not_modified_since.status, 304);
        assert_eq!(not_modified_since.header("etag"), Some(etag.as_str()));

        let failed_unmodified_since = proxy
            .request("GET /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nIf-Unmodified-Since: Thu, 01 Jan 1970 00:00:00 GMT\r\n")
            .await;
        assert_eq!(failed_unmodified_since.status, 412);

        let head_not_modified = proxy
            .request(&format!(
                "HEAD /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nIf-None-Match: {etag}\r\n"
            ))
            .await;
        assert_eq!(head_not_modified.status, 304);
        assert!(head_not_modified.body().is_empty());

        let unsupported_method = proxy
            .request(&format!(
                "POST /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nIf-None-Match: {etag}\r\nContent-Length: 0\r\n"
            ))
            .await;
        assert_eq!(unsupported_method.status, 405);

        let not_modified = proxy
            .request(&format!(
                "GET /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nIf-None-Match: {etag}\r\n"
            ))
            .await;
        assert_eq!(not_modified.status, 304);
        assert!(not_modified.body().is_empty());

        let unknown_range = proxy
            .request("GET /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nRange: widgets=0-1\r\n")
            .await;
        assert_eq!(unknown_range.status, 200);
        assert_eq!(unknown_range.body(), b"custom asset");

        let stale_if_range = proxy
            .request("GET /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nRange: bytes=0-1\r\nIf-Range: \"stale\"\r\n")
            .await;
        assert_eq!(stale_if_range.status, 200);
        assert_eq!(stale_if_range.body(), b"custom asset");

        let matching_if_range = proxy
            .request(&format!(
                "GET /assets/asset.custom HTTP/1.1\r\nHost: static.test\r\nRange: bytes=0-1\r\nIf-Range: {etag}\r\n"
            ))
            .await;
        assert_eq!(matching_if_range.status, 206);
        assert_eq!(matching_if_range.body(), b"cu");
        assert!(matching_if_range.header("content-encoding").is_none());

        let directory_redirect = proxy
            .request("GET /assets/directory HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(directory_redirect.status, 301);
        assert_eq!(directory_redirect.header("location"), Some("/assets/directory/"));
        let index = proxy
            .request("GET /assets/directory/ HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(index.body(), b"directory index");

        let forbidden_directory = proxy
            .request("GET /assets/empty-directory HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(forbidden_directory.status, 301);
        let forbidden_directory = proxy
            .request("GET /assets/empty-directory/ HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(forbidden_directory.status, 403);

        let fallback = proxy
            .request("GET /fallback/missing HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(fallback.body(), b"fallback");

        let inline_error = proxy
            .request("GET /assets/missing HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(inline_error.status, 404);
        assert_eq!(inline_error.body(), b"branded missing");
        assert_eq!(inline_error.header("server"), Some("nginx/test"));

        let custom_error = proxy
            .request("GET /status/missing HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(custom_error.status, 503);
        assert_eq!(custom_error.body(), b"custom 50x");
        assert_eq!(custom_error.header("x-static-policy"), Some("host-shaped"));
        assert!(custom_error.header("x-nginx-status-policy").is_none());

        let listing = proxy
            .request("GET /listing/ HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(listing.status, 200);
        assert!(listing.text().contains("listing-entry.bin"));
        assert!(listing.text().contains("2K"));

        let ranged = proxy
            .request("GET /assets/large.bin HTTP/1.1\r\nHost: static.test\r\nRange: bytes=1048576-1048591\r\n")
            .await;
        assert_eq!(ranged.status, 206);
        assert_eq!(ranged.header("content-length"), Some("16"));
        assert_eq!(
            ranged.header("content-range"),
            Some("bytes 1048576-1048591/17825792")
        );
        assert_eq!(ranged.body(), &[0; 16]);

        let unsatisfiable = proxy
            .request("GET /assets/large.bin HTTP/1.1\r\nHost: static.test\r\nRange: bytes=99999999-\r\n")
            .await;
        assert_eq!(unsatisfiable.status, 416);
        assert_eq!(unsatisfiable.body(), b"custom 50x");
        assert_eq!(
            unsatisfiable.header("content-range"),
            Some("bytes */17825792")
        );
        assert_eq!(unsatisfiable.header("x-static-policy"), Some("host-shaped"));
        assert!(unsatisfiable.header("x-nginx-status-policy").is_none());

        let root_mapped = proxy
            .request("GET /root/root.txt HTTP/1.1\r\nHost: static.test\r\n")
            .await;
        assert_eq!(root_mapped.body(), b"root mapped");

        proxy.finish().await;
    })
    .await
    .expect("extended static policy test timed out");
}

fn host_shaped_static_routes(root: &std::path::Path) -> Vec<HttpRoute> {
    vec![
        host_shaped_static_route(
            root,
            "/assets",
            HttpStaticPathMapping::Alias,
            Vec::new(),
            false,
            true,
        ),
        host_shaped_static_route(
            root,
            "/fallback",
            HttpStaticPathMapping::Alias,
            vec![
                HttpStaticTryFile::RequestPath,
                HttpStaticTryFile::Relative {
                    path: "fallback.txt".into(),
                },
            ],
            false,
            false,
        ),
        host_shaped_static_route(
            root,
            "/status",
            HttpStaticPathMapping::Alias,
            vec![
                HttpStaticTryFile::RequestPath,
                HttpStaticTryFile::Status { status: 503 },
            ],
            false,
            true,
        ),
        host_shaped_static_route(
            root,
            "/listing",
            HttpStaticPathMapping::Alias,
            Vec::new(),
            true,
            true,
        ),
        host_shaped_static_route(
            root,
            "/root",
            HttpStaticPathMapping::Root,
            Vec::new(),
            false,
            true,
        ),
    ]
}

fn host_shaped_static_route(
    root: &std::path::Path,
    path: &str,
    mapping: HttpStaticPathMapping,
    try_files: Vec<HttpStaticTryFile>,
    autoindex: bool,
    etag: bool,
) -> HttpRoute {
    HttpRoute {
        host: None,
        path: HttpPathSelector::SegmentPrefix { value: path.into() },
        methods: Vec::new(),
        access_policy: None,
        policy: oxiroute_config::HttpRoutePolicy::default(),
        action: HttpRouteAction::StaticFiles {
            root_directory: root.to_owned(),
            path_mapping: mapping,
            index_files: vec!["index.html".into()],
            internal_index_redirects: true,
            directory_redirects: true,
            spa_fallback: None,
            try_files,
            autoindex,
            autoindex_exact_size: false,
            autoindex_local_time: true,
            etag,
            mime: HttpStaticMimePolicy {
                default_type: Some("application/x-default".into()),
                types: vec![
                    HttpMimeType {
                        extension: "custom".into(),
                        content_type: "application/x-custom".into(),
                    },
                    HttpMimeType {
                        extension: "html".into(),
                        content_type: "text/html".into(),
                    },
                ],
            },
            headers: vec![
                HttpLiteralHeader {
                    name: "x-static-policy".into(),
                    value: "host-shaped".into(),
                    always: true,
                },
                HttpLiteralHeader {
                    name: "x-nginx-status-policy".into(),
                    value: "selected-statuses".into(),
                    always: false,
                },
            ],
            error_responses: vec![
                HttpStaticErrorResponse {
                    statuses: vec![416, 500, 502, 503, 504],
                    file: Some("50x.html".into()),
                    body: None,
                    headers: Vec::new(),
                    internal_redirect: None,
                },
                HttpStaticErrorResponse {
                    statuses: vec![404],
                    file: None,
                    body: Some("branded missing".into()),
                    headers: vec![HttpLiteralHeader {
                        name: "server".into(),
                        value: "nginx/test".into(),
                        always: true,
                    }],
                    internal_redirect: None,
                },
            ],
        },
    }
}
