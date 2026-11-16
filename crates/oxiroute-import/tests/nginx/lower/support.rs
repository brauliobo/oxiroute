use std::{fs, path::Path};

use oxiroute_import::nginx::import_http_fragment;
use tempfile::TempDir;

pub(super) fn fixture(name: &str) -> TempDir {
    let directory = tempfile::tempdir().expect("create fixture directory");
    let source = fs::read(Path::new("tests/fixtures").join(name)).expect("read fixture");
    let certificate =
        fs::canonicalize("tests/fixtures/nginx/proxy.pem").expect("canonical certificate fixture");
    let private_key = copy_secure_key(&directory, "proxy-key.pem", "proxy-key.pem");
    let source = String::from_utf8_lossy(&source)
        .replacen("http {", "http { access_log off;", 1)
        .replace("@CERTIFICATE@", &certificate.to_string_lossy())
        .replace("@PRIVATE_KEY@", &private_key.to_string_lossy());
    fs::write(directory.path().join("nginx.conf"), source).expect("write fixture");
    directory
}

pub(super) fn copy_secure_key(
    directory: &TempDir,
    fixture: &str,
    name: &str,
) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = directory.path().join(name);
    fs::copy(Path::new("tests/fixtures/nginx").join(fixture), &path).expect("copy test key");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("secure test key mode");
    path
}

pub(super) fn import_source(source: &str) -> oxiroute_import::nginx::ImportReport {
    let directory = tempfile::tempdir().expect("create source directory");
    let source = source.replacen("http {", "http { access_log off;", 1);
    fs::write(directory.path().join("nginx.conf"), source).expect("write source");
    import_http_fragment(Path::new("nginx.conf"), directory.path())
}
