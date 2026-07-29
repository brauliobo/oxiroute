use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fmt::Write as _,
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
};

use oxiroute_config::{RtmpRecorderStart, UpstreamTls};
use oxiroute_import::{
    DiagnosticStage, OperationalOverlayKind, Severity,
    haproxy::{
        BalanceAlgorithm, BindAddress, E_LOGGING_UNSUPPORTED, E_PROCESS_OWNED, E_STATS_UNSUPPORTED,
        HaproxyImportOptions, HaproxyOneRequestPerConnectionOverlay, PreprocessingEnvironment,
        ServerAddress, import_parsed_with_options, import_roots_with_environment, resolve_parsed,
    },
    nginx::{
        NginxBearerTokenOverlay, NginxDefaultAccessLogOverlay, NginxDefaultErrorPageOverlay,
        NginxHostTimezoneOverlay, NginxImportOptions, NginxRecordingRootOverlay,
        NginxUpstreamTlsOverlay, OccurrenceDisposition, import_root_with_options,
    },
};
use serde::Deserialize;
use syn::{Attribute, Item};

use crate::{
    report_invariants::{
        assert_haproxy_report_invariants, assert_import_report_invariants,
        assert_rtmp_import_report_invariants, diagnostic_count, import_nginx_fixture,
        import_rtmp_fixture, parse_haproxy_fixture,
    },
    squid_probes::assert_hostrouter_squid_inventory,
    support::{read_manifest, read_source, reference_parts, workspace_path},
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostManifest {
    schema_version: u32,
    source: String,
    direct_inventory: String,
    pub(crate) evidence: Vec<String>,
    required_gates: Vec<String>,
    audit: HostAudit,
    pub(crate) cases: Vec<HostCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostAudit {
    default: HostAuditStatus,
    fixtures: Vec<HostFixtureAudit>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum HostAuditStatus {
    NonAudited,
    LiveOriginHashedReadOnlyCaptured,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostFixtureAudit {
    path: String,
    status: HostAuditStatus,
    cases: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostCase {
    pub(crate) id: String,
    status: HostStatus,
    gates: HostGates,
    evidence: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum HostStatus {
    Covered,
    Partial,
    Missing,
    External,
    Inactive,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostGates {
    canonical: Gate,
    runtime: Gate,
    failure: Gate,
    tests: Gate,
    native_lowering: Gate,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(transparent)]
struct Gate(bool);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectHostInventory {
    schema_version: u32,
    audit_date: String,
    host: String,
    services: Vec<DirectServiceInventory>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectServiceInventory {
    product: String,
    status: String,
    version: Option<String>,
    sanitized_fixture: Option<String>,
    expanded_directives: usize,
    access_rules: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveFixtureMetadata {
    schema_version: u32,
    host: String,
    host_timezone: String,
    audit_status: HostAuditStatus,
    sanitized: bool,
    origin_captures: Vec<LiveOriginCapture>,
    sanitizer: LiveSanitizer,
    native_versions: HashMap<String, String>,
    native_version_availability: String,
    haproxy_environment: Option<LiveHaproxyEnvironment>,
    files: Vec<LiveFixtureFile>,
    overlay_inventory: Vec<LiveOverlayInventory>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveHaproxyEnvironment {
    node_ip: IpAddr,
    gpu1_defined: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveOriginCapture {
    product: String,
    captured_on: Option<String>,
    read_command: String,
    hash_command: String,
    sha256: Option<String>,
    availability: String,
    raw_bytes_stored: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveSanitizer {
    input: String,
    steps: Vec<String>,
    raw_bytes_stored: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveFixtureFile {
    path: String,
    post_sanitization_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveOverlayInventory {
    kind: String,
    count: usize,
}

#[test]
fn host_manifest_covers_the_documented_inventory_and_enforces_gates() {
    let manifest: HostManifest = read_manifest("host-cases.json");
    assert_eq!(manifest.schema_version, 3);
    assert_eq!(manifest.source, "docs/HOST_CONFIG_COVERAGE.md");
    assert_eq!(manifest.direct_inventory, "coverage/host-inventory.json");
    validate_direct_host_inventory(&manifest);
    assert_eq!(manifest.audit.default, HostAuditStatus::NonAudited);
    assert_eq!(
        manifest.required_gates,
        [
            "canonical",
            "runtime",
            "failure",
            "tests",
            "native_lowering"
        ]
    );

    let mut documented = documented_host_case_ids();
    documented.extend(["SQ-01", "HV-01"]);
    let actual = manifest
        .cases
        .iter()
        .map(|case| case.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual, documented,
        "host coverage manifest IDs differ from the audited Markdown inventory"
    );

    validate_host_fixture_probes(&manifest);
    for case in &manifest.cases {
        validate_host_case(case).unwrap_or_else(|error| panic!("{}: {error}", case.id));
    }
    assert_new_endpoint_case_gates(&manifest);
    assert_recording_case_gates(&manifest);
    assert_foundation_host_case_gates(&manifest);
}

#[test]
fn covered_host_status_rejects_one_missing_required_gate() {
    let case = HostCase {
        id: "TEST-01".into(),
        status: HostStatus::Covered,
        gates: HostGates {
            canonical: Gate(true),
            runtime: Gate(true),
            failure: Gate(true),
            tests: Gate(true),
            native_lowering: Gate(false),
        },
        evidence: vec![
            "crates/oxiroute-import/tests/coverage_manifests/host_ledger.rs#covered_host_status_rejects_one_missing_required_gate"
                .into(),
        ],
    };

    assert_eq!(
        validate_host_case(&case),
        Err("covered status requires every gate".into())
    );
}

fn documented_host_case_ids() -> BTreeSet<&'static str> {
    include_str!("../../../../docs/HOST_CONFIG_COVERAGE.md")
        .lines()
        .filter_map(|line| {
            let mut columns = line.split('|').skip(1).map(str::trim);
            let id = columns.next()?;
            is_host_case_id(id).then_some(id)
        })
        .collect()
}

fn assert_new_endpoint_case_gates(manifest: &HostManifest) {
    for id in ["HN-09", "HN-10", "HH-01", "HH-03", "HH-04"] {
        let case = manifest
            .cases
            .iter()
            .find(|case| case.id == id)
            .unwrap_or_else(|| panic!("missing endpoint host case {id}"));
        assert_eq!(case.status, HostStatus::Partial, "{id} status");
        assert!(case.gates.canonical.0, "{id} canonical gate");
        assert!(case.gates.runtime.0, "{id} runtime gate");
        assert!(case.gates.failure.0, "{id} failure gate");
        assert!(case.gates.tests.0, "{id} tests gate");
        assert!(!case.gates.native_lowering.0, "{id} native lowering gate");
    }
}

fn assert_recording_case_gates(manifest: &HostManifest) {
    for id in ["PR-03", "PR-05", "PR-06"] {
        let case = manifest
            .cases
            .iter()
            .find(|case| case.id == id)
            .unwrap_or_else(|| panic!("missing recording host case {id}"));
        assert_eq!(case.status, HostStatus::Covered, "{id} status");
        assert!(case.gates.canonical.0, "{id} canonical gate");
        assert!(case.gates.runtime.0, "{id} runtime gate");
        assert!(case.gates.failure.0, "{id} failure gate");
        assert!(case.gates.tests.0, "{id} tests gate");
        assert!(case.gates.native_lowering.0, "{id} native lowering gate");
    }
}

fn assert_foundation_host_case_gates(manifest: &HostManifest) {
    let squid = manifest
        .cases
        .iter()
        .find(|case| case.id == "SQ-01")
        .expect("Squid host case");
    assert_eq!(squid.status, HostStatus::Partial);
    assert!(!squid.gates.canonical.0);
    assert!(!squid.gates.runtime.0);
    assert!(squid.gates.failure.0);
    assert!(squid.gates.tests.0);
    assert!(!squid.gates.native_lowering.0);

    let varnish = manifest
        .cases
        .iter()
        .find(|case| case.id == "HV-01")
        .expect("Varnish host case");
    assert_eq!(varnish.status, HostStatus::External);
    assert!(varnish.evidence.iter().all(|evidence| {
        evidence.starts_with("coverage/host-inventory.json#")
            && !evidence.contains("fixtures/varnish")
    }));
}

fn validate_direct_host_inventory(manifest: &HostManifest) {
    let inventory: DirectHostInventory = crate::support::read_json(&manifest.direct_inventory);
    assert_eq!(inventory.schema_version, 1);
    assert_eq!(inventory.audit_date, "2026-07-22");
    assert_eq!(inventory.host, "hostrouter.lan");
    assert_eq!(inventory.services.len(), 2);
    let squid = inventory
        .services
        .iter()
        .find(|service| service.product == "squid")
        .expect("direct Squid inventory");
    assert_eq!(squid.status, "present");
    assert_eq!(squid.version.as_deref(), Some("7.6"));
    assert_eq!(
        squid.sanitized_fixture.as_deref(),
        Some("crates/oxiroute-import/tests/fixtures/squid/hostrouter-sanitized.conf")
    );
    assert_eq!(squid.expanded_directives, 35);
    assert_eq!(squid.access_rules, 9);
    let varnish = inventory
        .services
        .iter()
        .find(|service| service.product == "varnish")
        .expect("direct Varnish inventory");
    assert_eq!(varnish.status, "absent");
    assert!(varnish.version.is_none());
    assert!(varnish.sanitized_fixture.is_none());
    assert_eq!(varnish.expanded_directives, 0);
    assert_eq!(varnish.access_rules, 0);
}

fn is_host_case_id(id: &str) -> bool {
    let Some((prefix, ordinal)) = id.split_once('-') else {
        return false;
    };
    matches!(
        prefix,
        "IMP" | "HN" | "HH" | "PN" | "PR" | "PI" | "CI" | "BI" | "SQ" | "HV"
    ) && ordinal.len() == 2
        && ordinal.bytes().all(|byte| byte.is_ascii_digit())
}

fn validate_host_case(case: &HostCase) -> Result<(), String> {
    if case.evidence.is_empty() {
        return Err("case requires current evidence".into());
    }
    let mut has_executable_test_anchor = false;
    for evidence in &case.evidence {
        let path = evidence
            .split('#')
            .next()
            .expect("split always yields one part");
        if !workspace_path(path).exists() {
            return Err(format!("evidence path does not exist: {path}"));
        }
        if path.contains("/tests/")
            && Path::new(path)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
            && evidence.contains('#')
        {
            assert_host_test_reference(&case.id, evidence);
            has_executable_test_anchor = true;
        }
    }
    if case.gates.tests.0 && !has_executable_test_anchor {
        return Err("tests gate requires an executable Rust test anchor".into());
    }

    let gates = [
        case.gates.canonical.0,
        case.gates.runtime.0,
        case.gates.failure.0,
        case.gates.tests.0,
        case.gates.native_lowering.0,
    ];
    if case.status == HostStatus::Covered && !gates.into_iter().all(|passed| passed) {
        return Err("covered status requires every gate".into());
    }
    Ok(())
}

fn validate_host_fixture_probes(manifest: &HostManifest) {
    let known_cases = manifest
        .cases
        .iter()
        .map(|case| case.id.as_str())
        .collect::<HashSet<_>>();
    let mut probed = HashSet::new();
    for fixture in &manifest.audit.fixtures {
        assert!(
            !fixture.cases.is_empty(),
            "host fixture mapping must name at least one executable case: {}",
            fixture.path
        );
        assert!(
            fixture
                .path
                .starts_with("crates/oxiroute-import/tests/fixtures/"),
            "host audit must use a sanitized fixture: {}",
            fixture.path
        );
        assert!(workspace_path(&fixture.path).is_file());
        for case in &fixture.cases {
            assert!(
                known_cases.contains(case.as_str()),
                "unknown audited case {case}"
            );
            assert!(
                probed.insert(case.as_str()),
                "duplicate host fixture probe for {case}"
            );
            match fixture.path.as_str() {
                "crates/oxiroute-import/tests/fixtures/nginx/hostrouter-partial.conf" => {
                    assert_nginx_host_case(case);
                }
                "crates/oxiroute-import/tests/fixtures/haproxy/hostrouter-active.cfg" => {
                    assert_haproxy_host_case(case);
                }
                "crates/oxiroute-import/tests/fixtures/haproxy/phoenix-dormant.cfg" => {}
                "crates/oxiroute-import/tests/fixtures/nginx/phoenix-audited-partial.conf" => {
                    assert_phoenix_rtmp_case(case);
                }
                "crates/oxiroute-import/tests/fixtures/squid/hostrouter-sanitized.conf" => {
                    assert_eq!(case, "SQ-01");
                    assert_hostrouter_squid_inventory();
                }
                "crates/oxiroute-import/tests/fixtures/live/whitebeast/metadata.json" => {
                    assert_eq!(
                        fixture.status,
                        HostAuditStatus::LiveOriginHashedReadOnlyCaptured
                    );
                    assert_live_origin_hashed_fixture("whitebeast", case);
                }
                "crates/oxiroute-import/tests/fixtures/live/hostrouter/metadata.json" => {
                    assert_eq!(
                        fixture.status,
                        HostAuditStatus::LiveOriginHashedReadOnlyCaptured
                    );
                    assert_live_origin_hashed_fixture("hostrouter", case);
                }
                "crates/oxiroute-import/tests/fixtures/live/phoenix/metadata.json" => {
                    assert_eq!(
                        fixture.status,
                        HostAuditStatus::LiveOriginHashedReadOnlyCaptured
                    );
                    assert_live_origin_hashed_fixture("phoenix", case);
                }
                "crates/oxiroute-import/tests/fixtures/live/chicopc/metadata.json" => {
                    assert_eq!(
                        fixture.status,
                        HostAuditStatus::LiveOriginHashedReadOnlyCaptured
                    );
                    assert_live_origin_hashed_fixture("chicopc", case);
                }
                "crates/oxiroute-import/tests/fixtures/live/back1/metadata.json" => {
                    assert_eq!(
                        fixture.status,
                        HostAuditStatus::LiveOriginHashedReadOnlyCaptured
                    );
                    assert_live_origin_hashed_fixture("back1", case);
                }
                path => panic!("host fixture has no executable assertion: {path}"),
            }
        }
        if fixture.status == HostAuditStatus::LiveOriginHashedReadOnlyCaptured {
            assert_live_fixture_metadata(&fixture.path);
        }
    }
}

fn assert_host_test_reference(case: &str, reference: &str) {
    let (relative_path, function_name) = reference_parts(case, reference);
    let syntax = syn::parse_file(&read_source(relative_path))
        .unwrap_or_else(|error| panic!("parse host evidence {relative_path}: {error}"));
    let matches = syntax
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Fn(function) if function.sig.ident == function_name => Some(function),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "{case} evidence does not resolve exactly one test: {reference}"
    );
    assert!(
        matches[0].attrs.iter().any(is_test_attribute),
        "{case} evidence anchor is not an executable test: {reference}"
    );
}

fn is_test_attribute(attribute: &Attribute) -> bool {
    attribute
        .path()
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "test")
}

fn assert_live_fixture_metadata(metadata_path: &str) {
    let metadata: LiveFixtureMetadata = crate::support::read_json(metadata_path);
    assert_eq!(metadata.schema_version, 4);
    assert_eq!(
        metadata.audit_status,
        HostAuditStatus::LiveOriginHashedReadOnlyCaptured
    );
    assert!(metadata.sanitized);
    assert!(!metadata.host_timezone.is_empty());
    assert_origin_capture_metadata(&metadata);
    assert!(!metadata.sanitizer.raw_bytes_stored);
    assert!(
        metadata
            .sanitizer
            .input
            .contains("no unsanitized byte stream")
    );
    assert_eq!(metadata.sanitizer.steps.len(), 5);
    assert!(metadata.sanitizer.steps.iter().any(|step| {
        step.contains("post_sanitization_sha256") && step.contains("independently")
    }));
    assert_native_capture_metadata(&metadata);

    let metadata_file = workspace_path(metadata_path);
    let root = metadata_file.parent().expect("live fixture root");
    let declared = metadata
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<HashSet<_>>();
    let actual = fixture_files(root)
        .into_iter()
        .filter(|path| path != &metadata_file)
        .map(|path| {
            path.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect::<HashSet<_>>();
    assert_eq!(declared, actual, "{} fixture file inventory", metadata.host);
    for file in &metadata.files {
        assert_eq!(
            live_file_hash(root, file),
            file.post_sanitization_sha256,
            "{}: {}",
            metadata.host,
            file.path
        );
    }

    let mut actual_overlays = HashMap::<&str, usize>::new();
    if root.join("nginx").is_dir() {
        let report = import_root_with_options(
            Path::new("nginx.conf"),
            &root.join("nginx"),
            &live_options(&metadata.host),
        );
        for overlay in &report.candidate.operational_overlays {
            let kind = match overlay.kind {
                OperationalOverlayKind::CertificateMaterial => "certificate_material",
                OperationalOverlayKind::DefaultErrorPageMigration => "default_error_page_migration",
                OperationalOverlayKind::HostTimezone => "host_timezone",
                OperationalOverlayKind::OneRequestPerConnection => "one_request_per_connection",
                OperationalOverlayKind::PrometheusMigration => "prometheus_migration",
                OperationalOverlayKind::RecordingRootMigration => "recording_root_migration",
                OperationalOverlayKind::StructuredAccessLogMigration => {
                    "structured_access_log_migration"
                }
                OperationalOverlayKind::HtpasswdFile => "htpasswd_file",
                OperationalOverlayKind::BearerTokenFile => "bearer_token_file",
                OperationalOverlayKind::UpstreamTlsPolicy => "upstream_tls_policy",
            };
            *actual_overlays.entry(kind).or_default() += 1;
        }
    }
    let expected_overlays = metadata
        .overlay_inventory
        .iter()
        .map(|entry| (entry.kind.as_str(), entry.count))
        .collect::<HashMap<_, _>>();
    assert_eq!(
        actual_overlays, expected_overlays,
        "{} overlay inventory",
        metadata.host
    );
}

fn assert_native_capture_metadata(metadata: &LiveFixtureMetadata) {
    assert_eq!(metadata.native_version_availability, "recorded");
    let captured_products = metadata
        .origin_captures
        .iter()
        .map(|capture| capture.product.as_str())
        .collect::<HashSet<_>>();
    assert_eq!(captured_products.len(), metadata.native_versions.len());
    if captured_products.contains("nginx") {
        assert_eq!(
            metadata.native_versions.get("nginx").map(String::as_str),
            Some("1.30.4")
        );
    }
    if !captured_products.contains("haproxy") {
        return;
    }
    assert_eq!(
        metadata.native_versions.get("haproxy").map(String::as_str),
        Some("3.4.2")
    );
    if metadata.host == "hostrouter" {
        assert!(metadata.haproxy_environment.is_none());
        return;
    }
    let environment = metadata
        .haproxy_environment
        .as_ref()
        .expect("captured HAProxy environment");
    let (node_ip, gpu1_defined) = match metadata.host.as_str() {
        "whitebeast" => ("10.0.0.10", true),
        "phoenix" => ("10.0.0.11", false),
        "chicopc" => ("10.0.0.15", true),
        "back1" => ("10.0.0.7", true),
        host => panic!("unknown HAProxy environment host {host}"),
    };
    assert_eq!(environment.node_ip, node_ip.parse::<IpAddr>().unwrap());
    assert_eq!(environment.gpu1_defined, gpu1_defined);
}

fn assert_origin_capture_metadata(metadata: &LiveFixtureMetadata) {
    let captures = metadata
        .origin_captures
        .iter()
        .map(|capture| (capture.product.as_str(), capture))
        .collect::<HashMap<_, _>>();
    assert_eq!(captures.len(), metadata.origin_captures.len());
    for (product, capture) in captures {
        let target = match metadata.host.as_str() {
            "hostrouter" => "hostrouter.lan",
            "phoenix" => "phoenix.lan",
            "chicopc" => "chicopc.lan",
            "back1" => "back1.lan",
            "whitebeast" => "whitebeast",
            host => panic!("unknown live fixture host {host}"),
        };
        let expected_read = if metadata.host == "whitebeast" && product == "nginx" {
            "ssh -T whitebeast 'sudo -n nginx -T 2>&1'".to_owned()
        } else if metadata.host == "whitebeast" {
            "sudo -A cat /etc/haproxy/haproxy.cfg".to_owned()
        } else if product == "nginx" {
            format!("ssh -T {target} 'sudo -n nginx -T 2>&1'")
        } else {
            format!("ssh -T {target} 'sudo -n cat /etc/haproxy/haproxy.cfg'")
        };
        assert_eq!(capture.read_command, expected_read);
        let expected_hash_command = if metadata.host == "whitebeast" && product == "haproxy" {
            "sha256sum /etc/haproxy/haproxy.cfg".to_owned()
        } else {
            format!("{expected_read} | sha256sum")
        };
        assert_eq!(capture.hash_command, expected_hash_command);
        assert!(!capture.raw_bytes_stored);

        let expected_hash = match (metadata.host.as_str(), product) {
            ("whitebeast", "nginx") => {
                Some("e6832fc9bb18b1bfb9623f16f2959d438ae800922a4e3add9a5e1e87b0031f2f")
            }
            ("whitebeast", "haproxy") => {
                Some("24bfddb26022e2dfc9d778a0683666516fe6cb8521c6473949f84147578ffa27")
            }
            ("hostrouter", "nginx") => {
                Some("6ed001a3532b36fb12a97497a9cb96ce5bdbd50ba8516b0e7c3ba7ad24ec860a")
            }
            ("hostrouter", "haproxy") => {
                Some("13410dac9ee450b3979aae14a3b88eef094720e859e2e8a00dd2a65f40c14ffd")
            }
            ("phoenix", "nginx") => {
                Some("bce24fd2f2015f2f35f711a820aa2edbeda3dc6c991c669a6eec0662d21ee0b9")
            }
            ("phoenix" | "chicopc" | "back1", "haproxy") => {
                Some("4fbe3adb2f19cbf4991a5ef3d0176f6a4af7d4043c887a66abfe16d4d3cfa860")
            }
            _ => unreachable!(),
        };
        assert_eq!(capture.sha256.as_deref(), expected_hash);
        if expected_hash.is_some() {
            let expected_date = match (metadata.host.as_str(), product) {
                ("whitebeast", "haproxy") => "2026-07-27",
                ("chicopc" | "back1", "haproxy") => "2026-07-28",
                _ => "2026-07-26",
            };
            assert_eq!(capture.captured_on.as_deref(), Some(expected_date));
            assert_eq!(capture.availability, "recorded");
            assert!(capture.sha256.as_deref().is_some_and(is_sha256));
        } else {
            assert!(capture.captured_on.is_none());
            assert_eq!(capture.availability, "pending_live_recapture_after_change");
        }
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[test]
fn live_origin_hashes_pin_direct_read_only_commands_without_storing_raw_bytes() {
    for host in ["whitebeast", "hostrouter", "phoenix", "chicopc", "back1"] {
        let metadata: LiveFixtureMetadata = crate::support::read_json(format!(
            "crates/oxiroute-import/tests/fixtures/live/{host}/metadata.json"
        ));
        assert_origin_capture_metadata(&metadata);
    }
}

#[test]
fn checked_in_sanitized_hash_gate_rejects_per_file_drift() {
    let metadata: LiveFixtureMetadata = crate::support::read_json(
        "crates/oxiroute-import/tests/fixtures/live/phoenix/metadata.json",
    );
    let source_root = workspace_path("crates/oxiroute-import/tests/fixtures/live/phoenix");
    let source = &metadata.files[0];
    let directory = tempfile::tempdir().expect("fixture hash drift directory");
    let destination = directory.path().join(&source.path);
    fs::create_dir_all(destination.parent().unwrap()).expect("fixture hash parent");
    fs::copy(source_root.join(&source.path), &destination).expect("copy fixture hash source");
    assert_eq!(
        live_file_hash(directory.path(), source),
        source.post_sanitization_sha256
    );
    let mut bytes = fs::read(&destination).unwrap();
    bytes.push(b'\n');
    fs::write(&destination, bytes).unwrap();
    assert_ne!(
        live_file_hash(directory.path(), source),
        source.post_sanitization_sha256
    );
}

fn live_file_hash(root: &Path, file: &LiveFixtureFile) -> String {
    let bytes = fs::read(root.join(&file.path)).expect("read live fixture file");
    sha256_hex(&bytes)
}

fn fixture_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory).expect("read fixture directory") {
            let path = entry.expect("fixture directory entry").path();
            if path.is_dir() {
                directories.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in openssl::sha::sha256(bytes) {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn assert_live_origin_hashed_fixture(host: &str, case: &str) {
    if matches!(case, "PI-01" | "CI-01" | "BI-01") {
        let fixture_root =
            workspace_path(format!("crates/oxiroute-import/tests/fixtures/live/{host}"));
        let metadata: LiveFixtureMetadata = crate::support::read_json(format!(
            "crates/oxiroute-import/tests/fixtures/live/{host}/metadata.json"
        ));
        let environment = metadata
            .haproxy_environment
            .expect("captured HAProxy environment");
        let report = import_roots_with_environment(
            &[fixture_root.join("haproxy.cfg")],
            PreprocessingEnvironment {
                node_ip: environment.node_ip,
                gpu1_defined: environment.gpu1_defined,
            },
        );
        let config = report
            .value()
            .config
            .as_ref()
            .unwrap_or_else(|| panic!("{host} HAProxy fixture did not finalize"));
        assert_eq!(config.listeners.len(), 3);
        assert_eq!(config.upstream_pools.len(), 3);
        let expected_servers = if host == "phoenix" { 1 } else { 2 };
        assert!(
            config
                .upstream_pools
                .iter()
                .all(|pool| pool.servers.len() == expected_servers)
        );
        match (host, case) {
            ("phoenix", "PI-01") | ("chicopc", "CI-01") | ("back1", "BI-01") => {}
            (_, id) => panic!("live-origin fixture case has no assertion: {id}"),
        }
        return;
    }
    let root = workspace_path(format!(
        "crates/oxiroute-import/tests/fixtures/live/{host}/nginx"
    ));
    let report = import_root_with_options(Path::new("nginx.conf"), &root, &live_options(host));
    if host == "whitebeast" {
        assert!(report.candidate.config.is_none());
        match case {
            "HN-17" => assert!(
                report
                    .candidate
                    .operational_overlays
                    .iter()
                    .any(|overlay| overlay.kind == OperationalOverlayKind::HtpasswdFile)
            ),
            id => panic!("live-origin fixture case has no assertion: {id}"),
        }
        return;
    }
    if host == "hostrouter" {
        assert!(report.candidate.config.is_none());
        match case {
            "HN-11" => assert!(
                report
                    .candidate
                    .operational_overlays
                    .iter()
                    .any(|overlay| overlay.kind == OperationalOverlayKind::UpstreamTlsPolicy)
            ),
            "HN-12" => assert!(
                report
                    .candidate
                    .operational_overlays
                    .iter()
                    .filter(|overlay| overlay.kind == OperationalOverlayKind::UpstreamTlsPolicy)
                    .count()
                    >= 2
            ),
            "HN-19" => assert!(!report.diagnostics.iter().any(|diagnostic| {
                diagnostic.stage() == DiagnosticStage::Lower
                    && diagnostic.message().contains("proxy defaults")
            })),
            id => panic!("live-origin fixture case has no assertion: {id}"),
        }
        return;
    }
    assert!(report.candidate.config.is_some());
    let rtmp = &report.candidate.draft.rtmp_services[0];
    match case {
        "PR-03" => assert!(rtmp.applications[0].live),
        "PR-05" => assert!(!rtmp.applications[0].recorders.is_empty()),
        "PR-06" => assert!(!rtmp.applications[0].recorders[0].suffix_template.is_empty()),
        id => panic!("live-origin fixture case has no assertion: {id}"),
    }
}

fn live_options(host: &str) -> NginxImportOptions {
    let mut options = NginxImportOptions::default();
    if host == "phoenix" {
        options.host_timezones = vec![NginxHostTimezoneOverlay {
            timezone: "America/Bahia".into(),
        }];
        options.default_access_log = Some(NginxDefaultAccessLogOverlay {
            path: "/var/lib/oxiroute/http-access.jsonl".into(),
        });
        options.recording_root = Some(NginxRecordingRootOverlay {
            path: "/mnt/cloud/4tb/cam-rtmp".into(),
        });
        options.default_error_page = Some(NginxDefaultErrorPageOverlay {
            server: "nginx/1.30.2".into(),
        });
    }
    if host == "hostrouter" {
        options.upstream_tls = vec![
            live_tls_overlay("10.0.11.211", "phoenix.brauliobo.org", false),
            live_tls_overlay("phoenix.lan:4081", "phoenix.lan", true),
            live_tls_overlay("10.0.11.204", "nuvem.d4all.org", false),
        ];
        options.bearer_tokens = vec![NginxBearerTokenOverlay {
            server_name: "ollama.yellowmaverick.com".into(),
            token_file_path: "/run/secrets/ollama.token".into(),
        }];
    }
    options
}

fn live_tls_overlay(
    authority: &str,
    server_name: &str,
    activation: bool,
) -> NginxUpstreamTlsOverlay {
    NginxUpstreamTlsOverlay {
        authority: authority.into(),
        tls: UpstreamTls {
            server_name: server_name.into(),
            ca_certificate_path: None,
        },
        require_connectivity_activation: activation,
    }
}

fn assert_phoenix_rtmp_case(case: &str) {
    let report = import_rtmp_fixture("phoenix-audited-partial.conf");
    assert_rtmp_import_report_invariants(&report);
    assert!(report.config.is_none());
    assert_eq!(report.blocked_services.len(), 1);
    assert_eq!(report.draft.rtmp_services.len(), 1);
    let safe = &report.draft.rtmp_services[0].applications[0];
    assert_eq!(safe.name, "safe");
    assert!(safe.live);
    assert!(
        report
            .draft
            .rtmp_services
            .iter()
            .flat_map(|service| &service.applications)
            .all(|application| application.name != "phoenix")
    );

    match case {
        "PR-03" => assert!(report.occurrence_ledger.iter().any(|decision| {
            decision.name.value == b"live"
                && decision.disposition == OccurrenceDisposition::Resolved
        })),
        "PR-05" => {
            assert_eq!(safe.recorders.len(), 1);
            assert_eq!(safe.recorders[0].start, RtmpRecorderStart::Continuous);
        }
        "PR-06" => {
            let recorder = &safe.recorders[0];
            assert_eq!(
                recorder.root_directory,
                Path::new("/var/lib/oxiroute/safe-recordings")
            );
            assert_eq!(recorder.suffix_template, ".flv");
            assert!(!recorder.append_unix_seconds);
            assert!(report.occurrence_ledger.iter().any(|decision| {
                decision.name.value == b"record_suffix"
                    && decision.disposition == OccurrenceDisposition::Resolved
            }));
            assert!(report.occurrence_ledger.iter().any(|decision| {
                decision.name.value == b"record_max_size"
                    && matches!(decision.disposition, OccurrenceDisposition::Blocking(_))
            }));
        }
        id => panic!("Phoenix RTMP case has no fixture assertion: {id}"),
    }
}

fn assert_nginx_host_case(case: &str) {
    let report = import_nginx_fixture("hostrouter-partial.conf");
    assert_import_report_invariants(&report);
    match case {
        "HN-08" => assert!(report.occurrence_ledger.iter().any(|decision| {
            decision.name.value == b"server"
                && decision
                    .arguments
                    .iter()
                    .any(|argument| argument.value.starts_with(b"127.0.0.1:"))
                && decision.disposition == OccurrenceDisposition::Resolved
        })),
        "HN-09" => {
            assert!(report.blocked_services.is_empty());
            assert!(report.config.is_some());
            assert!(report.occurrence_ledger.iter().any(|decision| {
                decision.name.value == b"server"
                    && decision
                        .arguments
                        .iter()
                        .any(|argument| argument.value.starts_with(b"application.internal.lan:"))
                    && decision.disposition == OccurrenceDisposition::Resolved
            }));
            assert!(!report.diagnostics.iter().any(|diagnostic| {
                diagnostic.stage() == DiagnosticStage::Lower
                    && diagnostic.message().contains("static IP endpoint")
            }));
        }
        "HN-19" => {
            assert!(report.config.is_some());
            assert!(!report.diagnostics.iter().any(|diagnostic| {
                diagnostic.stage() == DiagnosticStage::Lower
                    && diagnostic.message().contains("proxy defaults")
            }));
        }
        id => panic!("nginx host case has no fixture assertion: {id}"),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one exhaustive match binds every authenticated HAProxy host case"
)]
fn assert_haproxy_host_case(case: &str) {
    let parsed = parse_haproxy_fixture("hostrouter-active.cfg");
    let resolved = resolve_parsed(parsed.clone());
    let lowered = import_parsed_with_options(
        parsed.clone(),
        &HaproxyImportOptions {
            one_request_per_connection: vec![HaproxyOneRequestPerConnectionOverlay {
                backend: "app_nodes".into(),
            }],
            prometheus_migrations: Vec::new(),
        },
    );
    assert_haproxy_report_invariants(parsed.value(), resolved.value());
    let effective = resolved.value();
    let diagnostics = lowered.diagnostics();
    match case {
        "HH-01" => {
            assert!(matches!(
                effective.frontends[0].binds[0].value,
                BindAddress::Unix { .. }
            ));
            assert_no_diagnostic_message(diagnostics, "Unix bind sockets");
        }
        "HH-02" => assert_eq!(effective.frontends[0].use_backends.len(), 1),
        "HH-03" => {
            assert_eq!(
                effective.backends[0]
                    .settings
                    .balance
                    .as_ref()
                    .unwrap()
                    .value,
                BalanceAlgorithm::LeastConnections
            );
            assert_no_diagnostic_message(diagnostics, "leastconn");
        }
        "HH-04" => {
            assert!(matches!(
                effective.backends[0].servers[0].address.value,
                ServerAddress::Tcp { ref host, .. } if host == b"app01.lan"
            ));
            assert_no_diagnostic_message(diagnostics, "DNS-named servers");
        }
        "HH-05" => {
            assert!(effective.backends[0].settings.http_check.is_some());
            assert!(effective.backends[0].settings.http_check_expect.is_some());
            assert_eq!(
                effective.backends[0].servers[0]
                    .rise
                    .as_ref()
                    .unwrap()
                    .value,
                2
            );
            assert_no_diagnostic_message(diagnostics, "initially eligible");
        }
        "HH-06" => {
            assert_eq!(
                effective.backends[0]
                    .settings
                    .retries
                    .as_ref()
                    .unwrap()
                    .value,
                3
            );
            assert!(effective.backends[0].settings.redispatch.is_some());
            assert_no_diagnostic_message(diagnostics, "redispatch persistence");
        }
        "HH-07" => {
            let timeouts = &effective.frontends[0].settings.timeouts;
            assert!(timeouts.client.is_some());
            assert!(timeouts.connect.is_some());
            assert!(timeouts.server.is_some());
            assert!(timeouts.http_request.is_some());
            assert!(timeouts.http_keep_alive.is_some());
        }
        "HH-08" => {
            assert!(effective.frontends[0].settings.forward_for.is_some());
            assert_no_diagnostic_message(diagnostics, "forwardfor header insertion");
        }
        "HH-09" => {
            assert_eq!(effective.global.maxconn.as_ref().unwrap().value, 4096);
            assert_eq!(
                effective.frontends[0]
                    .settings
                    .maxconn
                    .as_ref()
                    .unwrap()
                    .value,
                2000
            );
        }
        "HH-10" => {
            assert_eq!(diagnostic_count(diagnostics, E_STATS_UNSUPPORTED), 6);
            assert!(lowered.value().draft.stats.is_none());
            assert_eq!(lowered.value().activation_requirements.len(), 6);
        }
        "HH-11" => assert_eq!(diagnostic_count(diagnostics, E_LOGGING_UNSUPPORTED), 3),
        "HH-12" => assert_external_process_settings(diagnostics),
        id => panic!("HAProxy host case has no fixture assertion: {id}"),
    }
}

fn assert_external_process_settings(diagnostics: &[oxiroute_import::Diagnostic]) {
    assert_eq!(diagnostic_count(diagnostics, E_PROCESS_OWNED), 4);
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic.code() == E_PROCESS_OWNED && diagnostic.severity() == Severity::Warning
            })
            .count(),
        4
    );
}

fn assert_no_diagnostic_message(diagnostics: &[oxiroute_import::Diagnostic], unexpected: &str) {
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.message().contains(unexpected)),
        "unexpected diagnostic containing {unexpected:?}"
    );
}
