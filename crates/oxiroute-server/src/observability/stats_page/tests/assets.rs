use std::{fs, path::Path};

use oxiroute_config::StatsPageAdminPolicy::Disabled;

use super::fixture::{fixture, page, page_config};
use crate::{
    GenerationManager,
    observability::stats_page::{
        HaproxyStatsPage,
        assets::{self, Asset},
        page::secure_page_response,
    },
};

const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const ASSET_DIRECTORY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/stats");

fn first_asset() -> &'static Asset {
    assets::all()
        .first()
        .expect("the embedded bundle has an asset")
}

fn asset_path(asset: &Asset) -> String {
    format!("/haproxy/assets/{}", asset.name)
}

#[tokio::test]
async fn every_embedded_asset_is_served_with_its_bytes_type_and_validator() {
    let fixture = fixture();
    let page = page(&fixture, Disabled);
    for asset in assets::all() {
        let response = page.handle("GET", &asset_path(asset), false).await;
        assert_eq!(response.status, 200, "{}", asset.name);
        assert_eq!(response.body, asset.body, "{}", asset.name);
        assert_eq!(response.etag.as_deref(), Some(asset.etag), "{}", asset.name);
        assert_ne!(
            response.content_type, "application/octet-stream",
            "{}",
            asset.name
        );
        assert_eq!(response.cache_control, Some(IMMUTABLE), "{}", asset.name);
    }
}

#[test]
fn content_types_follow_the_extension() {
    for (name, content_type) in [
        ("a-12345678.js", "text/javascript; charset=utf-8"),
        ("a-12345678.css", "text/css; charset=utf-8"),
        ("a-12345678.svg", "image/svg+xml"),
        ("a-12345678.png", "image/png"),
        ("a-12345678.ico", "image/x-icon"),
        ("a-12345678.woff2", "font/woff2"),
        ("a-12345678.bin", "application/octet-stream"),
        ("js", "application/octet-stream"),
    ] {
        assert_eq!(assets::content_type(name), content_type, "{name}");
    }
}

#[test]
fn the_embedded_table_is_the_asset_directory() {
    let directory = Path::new(ASSET_DIRECTORY);
    assert_eq!(
        assets::shell().as_bytes(),
        fs::read(directory.join("index.html")).expect("shell")
    );
    let mut on_disk: Vec<_> = fs::read_dir(directory.join("assets"))
        .expect("asset directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .into_string()
                .expect("UTF-8 name")
        })
        .collect();
    on_disk.sort();
    let embedded: Vec<_> = assets::all().iter().map(|asset| asset.name).collect();
    assert_eq!(embedded, on_disk);
    for asset in assets::all() {
        let bytes = fs::read(directory.join("assets").join(asset.name)).expect("asset");
        assert_eq!(asset.body, bytes, "{}", asset.name);
    }
}

#[test]
fn every_asset_name_ends_in_a_content_hash() {
    for asset in assets::all() {
        let (stem, extension) = asset.name.rsplit_once('.').expect("extension");
        let (_, hash) = stem.rsplit_once('-').expect("hash separator");
        assert!(
            hash.len() >= 8
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "{}",
            asset.name
        );
        assert!(!extension.is_empty());
    }
}

#[tokio::test]
async fn unknown_and_traversing_asset_paths_are_not_found() {
    let fixture = fixture();
    let page = page(&fixture, Disabled);
    let name = first_asset().name;
    for path in [
        "/haproxy/assets".to_owned(),
        "/haproxy/assets/".to_owned(),
        "/haproxy/assets/missing-12345678.js".to_owned(),
        format!("/haproxy/assets/{name}/"),
        format!("/haproxy/assets/{name}.map"),
        format!("/haproxy/assets//{name}"),
        format!("/haproxy/assets/sub/{name}"),
        format!("/haproxy/assets/./{name}"),
        format!("/haproxy/assets/../assets/{name}"),
        "/haproxy/assets/../index.html".to_owned(),
        "/haproxy/assets/../../src/main.rs".to_owned(),
        "/haproxy/assets/%2e%2e/index.html".to_owned(),
        format!("/haproxy/assets/..%2fassets%2f{name}"),
        format!("/haproxy/assets/%2e%2e%2fassets%2f{name}"),
        format!("/haproxy/assets\\{name}"),
        format!("/haproxy/assets/{name}%00.js"),
        format!("/haproxy/assets/{}", name.to_uppercase()),
        format!("/assets/{name}"),
        format!("/haproxyfoo/assets/{name}"),
        "/haproxy/index.html".to_owned(),
        format!("/haproxy/assets{name}"),
    ] {
        assert_eq!(page.handle("GET", &path, true).await.status, 404, "{path}");
    }
}

#[tokio::test]
async fn assets_answer_get_and_head_only_and_head_keeps_the_get_headers() {
    let fixture = fixture();
    let page = page(&fixture, Disabled);
    let path = asset_path(first_asset());
    let get = secure_page_response(page.handle("GET", &path, false).await);
    let mut head = page.handle("HEAD", &path, false).await;
    assert!(head.body.is_empty());
    head.body = first_asset().body.to_vec();
    let head = secure_page_response(head);
    for name in ["content-type", "content-length", "etag", "cache-control"] {
        assert_eq!(head.headers()[name], get.headers()[name], "{name}");
    }
    for method in ["POST", "PUT", "DELETE", "PATCH", "OPTIONS"] {
        let response = page.handle(method, &path, true).await;
        assert_eq!(
            (response.status, response.allow),
            (405, Some("GET, HEAD")),
            "{method}"
        );
    }
}

#[tokio::test]
async fn a_matching_validator_answers_304_without_a_body_and_keeps_the_policy() {
    let fixture = fixture();
    let page = page(&fixture, Disabled);
    let asset = first_asset();
    let path = asset_path(asset);
    for validator in [
        asset.etag.to_owned(),
        format!("W/{}", asset.etag),
        format!("\"x\", {}", asset.etag),
        "*".into(),
    ] {
        let response = page
            .handle_conditional("GET", &path, false, Some(&validator))
            .await;
        assert_eq!(
            (response.status, response.body.len()),
            (304, 0),
            "{validator}"
        );
        let secured = secure_page_response(response);
        assert_eq!(secured.headers()["etag"], asset.etag);
        assert_eq!(secured.headers()["cache-control"], IMMUTABLE);
        assert!(secured.headers().get("content-length").is_none());
    }
    let stale = page
        .handle_conditional("GET", &path, false, Some("\"stale\""))
        .await;
    assert_eq!(stale.status, 200);
}

#[tokio::test]
async fn assets_need_no_snapshot_and_never_build_one() {
    let page = HaproxyStatsPage::new(&page_config(1_000, Disabled), GenerationManager::new());
    let response = page.handle("GET", &asset_path(first_asset()), false).await;
    assert_eq!(response.status, 200);
}

#[tokio::test]
async fn the_shell_is_static_html_that_loads_only_embedded_script_files() {
    let fixture = fixture();
    let page = page(&fixture, Disabled);
    let response = page.handle("GET", "/haproxy", false).await;
    let shell = String::from_utf8(response.body).expect("HTML");
    assert_eq!(response.cache_control, None, "the shell is never cached");
    assert!(shell.starts_with("<!doctype html>"));
    assert!(!shell.contains("__stats_"), "{shell}");
    // Nothing runs inline: every script is a file, no handler attributes, no style element.
    for forbidden in [
        "javascript:",
        "<style",
        " style=",
        "<iframe",
        "<object",
        "<embed",
    ] {
        assert!(!shell.contains(forbidden), "{forbidden}");
    }
    assert!(!shell.split('<').any(|tag| {
        tag.split_whitespace()
            .skip(1)
            .any(|attribute| attribute.starts_with("on"))
    }));
    let scripts: Vec<_> = shell.split("<script").skip(1).collect();
    assert!(!scripts.is_empty());
    for script in scripts {
        let tag = &script[..script.find('>').unwrap()];
        let source = tag
            .split("src=\"")
            .nth(1)
            .expect("script src")
            .split('"')
            .next()
            .unwrap();
        let name = source
            .strip_prefix("/haproxy/assets/")
            .expect("served under the prefix");
        assert!(
            assets::all().iter().any(|asset| asset.name == name),
            "{source}"
        );
        assert!(
            script[tag.len()..].starts_with("></script>"),
            "inline script body"
        );
    }
    // The favicon is an empty data URL; every other link is an embedded stylesheet.
    for link in shell
        .split("<link")
        .skip(1)
        .filter(|link| !link.contains("href=\"data:"))
    {
        let tag = &link[..link.find('>').unwrap()];
        let source = tag.split("href=\"").nth(1).expect("link href");
        let name = source
            .split('"')
            .next()
            .and_then(|source| source.strip_prefix("/haproxy/assets/"))
            .expect("served under the prefix");
        assert!(
            assets::all().iter().any(|asset| asset.name == name),
            "{tag}"
        );
    }
}

/// A browser with script runs the client, which refreshes itself and draws its own table: the shell's meta refresh and
/// the server-rendered table are for browsers without script and must not show or act otherwise.
#[tokio::test]
async fn the_refresh_and_the_table_are_only_for_browsers_without_script() {
    let fixture = fixture();
    let page = page(&fixture, Disabled);
    let shell = String::from_utf8(page.handle("GET", "/haproxy", false).await.body).expect("HTML");
    for fallback in ["http-equiv=\"refresh\"", "<table", "<td>origin-a</td>"] {
        let at = shell
            .find(fallback)
            .unwrap_or_else(|| panic!("{fallback}: {shell}"));
        let before = &shell[..at];
        let open = before.rfind("<noscript>");
        assert!(open.is_some(), "{fallback} is outside <noscript>");
        assert!(
            before.rfind("</noscript>") < open,
            "{fallback} is outside <noscript>"
        );
    }
    assert!(
        shell.contains("<div id=\"app\"></div>"),
        "the client's root starts empty"
    );
}

#[tokio::test]
async fn the_shell_points_at_the_assets_below_the_prefix() {
    let fixture = fixture();
    let mut config = page_config(2_500, Disabled);
    for (prefix, expected) in [("/haproxy", "/haproxy"), ("/stats/", "/stats"), ("/", "")] {
        config.uri_prefix = prefix.into();
        let page = HaproxyStatsPage::new(&config, fixture.generations.clone());
        let shell = String::from_utf8(page.handle("GET", prefix, false).await.body).unwrap();
        assert!(
            shell.contains(&format!("name=\"oxiroute-prefix\" content=\"{expected}\"")),
            "{prefix}"
        );
        for asset in assets::all() {
            let script = assets::content_type(asset.name).starts_with("text/javascript");
            let attribute = if script { "src" } else { "href" };
            let path = format!("{expected}/assets/{}", asset.name);
            assert!(
                shell.contains(&format!("{attribute}=\"{path}\"")),
                "{prefix}: {shell}"
            );
            assert_eq!(page.handle("GET", &path, false).await.status, 200, "{path}");
        }
    }
}

#[tokio::test]
async fn a_hostile_prefix_cannot_break_out_of_the_shell() {
    let fixture = fixture();
    let mut config = page_config(1_000, Disabled);
    config.uri_prefix = "/\"><script>x</script>".into();
    let page = HaproxyStatsPage::new(&config, fixture.generations.clone());
    let shell =
        String::from_utf8(page.handle("GET", &config.uri_prefix, false).await.body).unwrap();
    assert!(!shell.contains("<script>x"), "{shell}");
}
