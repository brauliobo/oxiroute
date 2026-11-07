use std::{fs, path::PathBuf};

use crate::E_SOURCE_CHANGED;

use super::{VarnishLoadLimits, load_inner};

#[test]
fn final_recheck_detects_source_and_glob_changes() {
    let directory = temporary_directory();
    let includes = directory.join("conf.d");
    fs::create_dir_all(&includes).expect("include directory");
    let root = directory.join("root.vcl");
    fs::write(
        &root,
        b"vcl 4.1; include +glob \"conf.d/*.vcl\"; sub vcl_recv { return(hash); }",
    )
    .expect("root fixture");
    fs::write(
        includes.join("10-base.vcl"),
        b"sub vcl_hash { return(lookup); }",
    )
    .expect("include fixture");

    let changed_root = root.clone();
    let changed_includes = includes.clone();
    let report = load_inner(&root, VarnishLoadLimits::default(), move || {
        fs::write(changed_root, b"vcl 4.1; sub vcl_recv { return(pass); }").expect("changed root");
        fs::write(
            changed_includes.join("20-added.vcl"),
            b"sub vcl_deliver { return(deliver); }",
        )
        .expect("added include");
    });

    assert!(!report.value().snapshot_stable);
    assert!(
        report
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code() == E_SOURCE_CHANGED)
            .count()
            >= 2
    );
    fs::remove_dir_all(directory).expect("remove fixture directory");
}

fn temporary_directory() -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "oxiroute-varnish-loader-{}-{nonce}",
        std::process::id()
    ))
}
