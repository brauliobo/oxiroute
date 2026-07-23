use std::{
    collections::{BTreeSet, HashSet},
    path::Path,
};

use oxiroute_config::RtmpRecorderStart;
use oxiroute_import::{
    DiagnosticStage, Severity,
    haproxy::{
        BalanceAlgorithm, BindAddress, E_LOGGING_UNSUPPORTED, E_PROCESS_OWNED, E_STATS_UNSUPPORTED,
        ServerAddress, import_parsed as import_haproxy, resolve_parsed,
    },
    nginx::OccurrenceDisposition,
};
use serde::Deserialize;

use crate::{
    report_invariants::{
        assert_diagnostic_message, assert_haproxy_report_invariants,
        assert_import_report_invariants, assert_rtmp_import_report_invariants, diagnostic_count,
        import_nginx_fixture, import_rtmp_fixture, parse_haproxy_fixture,
    },
    squid_probes::assert_hostrouter_squid_inventory,
    support::{read_manifest, workspace_path},
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

#[test]
fn host_manifest_covers_the_documented_inventory_and_enforces_gates() {
    let manifest: HostManifest = read_manifest("host-cases.json");
    assert_eq!(manifest.schema_version, 1);
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
        evidence: vec!["coverage/host-cases.json".into()],
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
        assert_eq!(case.status, HostStatus::Partial, "{id} status");
        assert!(case.gates.canonical.0, "{id} canonical gate");
        assert!(case.gates.runtime.0, "{id} runtime gate");
        assert!(case.gates.failure.0, "{id} failure gate");
        assert!(case.gates.tests.0, "{id} tests gate");
        assert!(!case.gates.native_lowering.0, "{id} native lowering gate");
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
    matches!(prefix, "IMP" | "HN" | "HH" | "PN" | "PR" | "SQ" | "HV")
        && ordinal.len() == 2
        && ordinal.bytes().all(|byte| byte.is_ascii_digit())
}

fn validate_host_case(case: &HostCase) -> Result<(), String> {
    if case.evidence.is_empty() {
        return Err("case requires current evidence".into());
    }
    for evidence in &case.evidence {
        let path = evidence
            .split('#')
            .next()
            .expect("split always yields one part");
        if !workspace_path(path).exists() {
            return Err(format!("evidence path does not exist: {path}"));
        }
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
        assert_eq!(fixture.status, HostAuditStatus::NonAudited);
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
                path => panic!("host fixture has no executable assertion: {path}"),
            }
        }
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
            for directive in [b"record_suffix".as_slice(), b"record_max_size".as_slice()] {
                assert!(report.occurrence_ledger.iter().any(|decision| {
                    decision.name.value == directive
                        && matches!(decision.disposition, OccurrenceDisposition::Blocking(_))
                }));
            }
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
            assert_eq!(report.blocked_services.len(), 2);
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
            assert!(report.diagnostics.iter().any(|diagnostic| {
                diagnostic.stage() == DiagnosticStage::Lower
                    && diagnostic.message().contains("proxy defaults")
            }));
        }
        id => panic!("nginx host case has no fixture assertion: {id}"),
    }
}

fn assert_haproxy_host_case(case: &str) {
    let parsed = parse_haproxy_fixture("hostrouter-active.cfg");
    let resolved = resolve_parsed(parsed.clone());
    let lowered = import_haproxy(parsed.clone());
    assert_haproxy_report_invariants(parsed.value(), resolved.value());
    let effective = resolved.value();
    let diagnostics = lowered.diagnostics();
    assert_remaining_audited_haproxy_blockers(diagnostics);
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
            assert_diagnostic_message(diagnostics, "leastconn");
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
            assert_diagnostic_message(diagnostics, "initially eligible");
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
            assert_diagnostic_message(diagnostics, "redispatch persistence");
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
            assert_diagnostic_message(diagnostics, "forwardfor header insertion");
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
        "HH-10" => assert_eq!(diagnostic_count(diagnostics, E_STATS_UNSUPPORTED), 6),
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

fn assert_remaining_audited_haproxy_blockers(diagnostics: &[oxiroute_import::Diagnostic]) {
    for message in [
        "aggregate process limit",
        "leastconn",
        "initially eligible",
        "HAProxy retries",
        "redispatch persistence",
        "timeout scope",
        "forwardfor header insertion",
    ] {
        assert_diagnostic_message(diagnostics, message);
    }
}

fn assert_no_diagnostic_message(diagnostics: &[oxiroute_import::Diagnostic], unexpected: &str) {
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.message().contains(unexpected)),
        "unexpected diagnostic containing {unexpected:?}"
    );
}
