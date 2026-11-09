//! Every caller of the shared connector parses the system CA bundle once per process.
//! Runs the counting in a child process so `/proc/self/io` only sees this test's reads.
use oxiroute_tls_client::system_trust_connector;

const CALLERS: usize = 20;

fn bytes_read() -> u64 {
    std::fs::read_to_string("/proc/self/io")
        .expect("process io accounting")
        .lines()
        .find_map(|line| line.strip_prefix("rchar: "))
        .expect("rchar line")
        .parse()
        .expect("rchar value")
}

#[test]
#[ignore = "run by connectors_share_one_system_ca_parse"]
fn child_counts_ca_bundle_parses() {
    let bundle_bytes = std::fs::metadata(std::env::var_os("SSL_CERT_FILE").unwrap())
        .expect("bundle size")
        .len();
    let before = bytes_read();
    for _ in 0..CALLERS {
        system_trust_connector().expect("shared connector");
    }
    let parses = (bytes_read() - before) / bundle_bytes;
    assert!(parses <= 1, "the CA bundle was parsed {parses} times");
}

#[test]
fn connectors_share_one_system_ca_parse() {
    let bundle = tempfile::NamedTempFile::new().expect("bundle file");
    let ca = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../oxiroute-server/tests/fixtures/ca-a.pem"
    ))
    .expect("CA fixture");
    std::fs::write(bundle.path(), ca.repeat(1500)).expect("write bundle");
    let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args(["--ignored", "--exact", "child_counts_ca_bundle_parses"])
        .env("SSL_CERT_FILE", bundle.path())
        .output()
        .expect("child test");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
