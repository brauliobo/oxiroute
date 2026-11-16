use std::{fs, path::Path};

use oxiroute_import::nginx::import_rtmp_with_timezone;
use tempfile::TempDir;

pub(super) fn import_source(
    root: &[u8],
    includes: &[(&str, &[u8])],
) -> oxiroute_import::nginx::RtmpImportReport {
    let directory = TempDir::new().expect("RTMP source directory");
    fs::write(directory.path().join("nginx.conf"), root).expect("write RTMP root");
    for (name, contents) in includes {
        fs::write(directory.path().join(name), contents).expect("write RTMP include");
    }
    import_rtmp_with_timezone(
        Path::new("nginx.conf"),
        directory.path(),
        "America/Argentina/Buenos_Aires",
    )
}
