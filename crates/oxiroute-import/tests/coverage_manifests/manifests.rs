use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::Path,
};

use oxiroute_rtmp::{
    directive_specs, validate_directive, DirectiveContext, DirectiveError, DirectiveSpec,
    RelayKind, RuntimeSupport, ValueKind,
};
use serde::Deserialize;
use syn::{Attribute, Item};

use crate::support::{
    assert_nonempty_unique, assert_set_equality, assert_unique_ids, read_manifest, read_source,
    reference_parts, workspace_path,
};

const TEST_CATEGORIES: [&str; 18] = [
    "cli",
    "decode",
    "differential",
    "failure",
    "host_case",
    "integration",
    "interop",
    "normalize",
    "native_reference",
    "observability",
    "parse",
    "provenance",
    "reload",
    "render",
    "resolve",
    "security",
    "validate",
    "validation",
];

const TARGETS: [&str; 30] = [
    "config.loader",
    "deployment",
    "health.probe",
    "haproxy.effective_configuration",
    "http.access",
    "http.access-log",
    "http.gzip",
    "http.proxy",
    "http.router",
    "http.static",
    "import.blocking_diagnostic",
    "l4.relay",
    "listener.plan",
    "listener.http",
    "management.assets",
    "management.listener",
    "nginx.http_ir",
    "nginx.stream_ir",
    "nginx.source_graph",
    "operations.stats",
    "operations.stats-admin",
    "operations.stats-listener",
    "rtmp.directive_registry",
    "rtmp.listener",
    "rtmp.recorder",
    "rtmp.session",
    "schema",
    "apache.source_graph",
    "squid.semantic_ir",
    "tls.identity",
];

const ADDITIONAL_TARGETS: [&str; 23] = [
    "health.passive",
    "forward.access",
    "forward.admission",
    "forward.audit",
    "forward.auth",
    "forward.connect",
    "forward.destination",
    "forward.headers",
    "forward.listener",
    "forward.protocol",
    "forward.peer_policy",
    "forward.request",
    "forward.resolver",
    "forward.service",
    "forward.timeout",
    "forward.tunnel",
    "rtmp.access_policy",
    "rtmp.application",
    "rtmp.exec_profiles",
    "rtmp.session_limits",
    "rtmp.service",
    "tls.profile",
    "upstream.pool",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanonicalManifest {
    pub(crate) schema_version: u32,
    pub(crate) source: CanonicalSource,
    pub(crate) evidence: Vec<String>,
    pub(crate) entries: Vec<CanonicalEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanonicalSource {
    #[serde(rename = "crate")]
    pub(crate) crate_name: String,
    pub(crate) root_type: String,
    pub(crate) config_version: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanonicalEntry {
    pub(crate) id: String,
    pub(crate) kind: EntryKind,
    pub(crate) path: String,
    pub(crate) editability: Editability,
    pub(crate) normalization: Vec<String>,
    pub(crate) validation: Vec<String>,
    pub(crate) runtime: RuntimeDecision,
    pub(crate) required_tests: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EntryKind {
    Field,
    Enum,
    Variant,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Editability {
    Fixed,
    Operator,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeDecision {
    pub(crate) target: String,
    pub(crate) disposition: Disposition,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Disposition {
    Lowered,
    Classified,
    Externalized,
    Blocked,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectiveManifest<E> {
    pub(crate) schema_version: u32,
    pub(crate) product: String,
    pub(crate) reference: ReferencePin,
    pub(crate) evidence: Vec<String>,
    pub(crate) entries: Vec<E>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceManifest {
    schema_version: u32,
    entries: Vec<EvidenceEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceEntry {
    id: String,
    tests: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReferencePin {
    pub(crate) kind: String,
    pub(crate) value: String,
    pub(crate) documentation: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectiveForm {
    pub(crate) id: String,
    pub(crate) key: String,
    pub(crate) form: String,
    pub(crate) contexts: Vec<String>,
    pub(crate) disposition: Disposition,
    pub(crate) target: String,
    pub(crate) required_tests: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SquidDirectiveManifest {
    pub(crate) schema_version: u32,
    pub(crate) product: String,
    pub(crate) reference: ReferencePin,
    pub(crate) registry_version: u32,
    pub(crate) target_version: String,
    pub(crate) profile: SquidProfile,
    pub(crate) parity: CapabilityStatus,
    pub(crate) complete_parity: bool,
    pub(crate) evidence: Vec<String>,
    pub(crate) audit: SquidAudit,
    pub(crate) families: Vec<SquidFamilyManifest>,
    pub(crate) entries: Vec<SquidDirectiveForm>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SquidProfile {
    pub(crate) id: String,
    pub(crate) version: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SquidFamilyManifest {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) status: CapabilityStatus,
    pub(crate) rationale: String,
    pub(crate) current_evidence: Vec<String>,
    pub(crate) required_tests: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SquidDirectiveForm {
    pub(crate) id: String,
    pub(crate) key: String,
    pub(crate) family: String,
    pub(crate) form: String,
    pub(crate) contexts: Vec<String>,
    pub(crate) status: CapabilityStatus,
    pub(crate) rationale: String,
    pub(crate) current_evidence: Vec<String>,
    pub(crate) disposition: Disposition,
    pub(crate) target: String,
    pub(crate) required_tests: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CapabilityStatus {
    Compatible,
    Partial,
    Unsupported,
    Obsolete,
    NotPlanned,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SquidAudit {
    pub(crate) fixture: String,
    pub(crate) expanded_directives: usize,
    pub(crate) access_rules: usize,
    pub(crate) canonical_claim: bool,
    pub(crate) runtime_claim: bool,
    pub(crate) complete_parity_claim: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComponentManifest {
    pub(crate) schema_version: u32,
    pub(crate) evidence: Vec<String>,
    pub(crate) entries: Vec<ComponentEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComponentEntry {
    pub(crate) id: String,
    pub(crate) status: ComponentStatus,
    pub(crate) gates: ComponentGates,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ComponentStatus {
    Foundation,
    Integrated,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComponentGates {
    pub(crate) component: Gate,
    pub(crate) canonical: Gate,
    pub(crate) integrated_runtime: Gate,
    pub(crate) failure: Gate,
    pub(crate) tests: Gate,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(transparent)]
pub(crate) struct Gate(pub(crate) bool);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RtmpDirectiveEntry {
    id: String,
    key: String,
    contexts: Vec<RtmpContext>,
    min_args: u8,
    max_args: Option<u8>,
    value_kind: RtmpValueKind,
    #[serde(default)]
    values: Vec<String>,
    #[serde(default)]
    relay_kind: Option<RtmpRelayKind>,
    default: Option<String>,
    repeatable: bool,
    runtime_status: RtmpRuntimeStatus,
    disposition: Disposition,
    target: String,
    required_tests: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RtmpDirectiveManifest {
    schema_version: u32,
    product: String,
    reference: ReferencePin,
    evidence: Vec<String>,
    entries: Vec<RtmpDirectiveEntry>,
    import_forms: Vec<RtmpImportForm>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RtmpImportForm {
    id: String,
    key: String,
    form: String,
    contexts: Vec<RtmpContext>,
    disposition: Disposition,
    target: String,
    required_tests: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(rename_all = "snake_case")]
enum RtmpContext {
    NginxMain,
    RtmpMain,
    RtmpServer,
    RtmpApplication,
    RtmpRecorder,
    Http,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum RtmpValueKind {
    AccessLog,
    AccessRule,
    Bitmask,
    Block,
    Command,
    Duration,
    DurationOrOff,
    Enum,
    Flag,
    HlsVariant,
    Integer,
    Listen,
    LogFormat,
    NamedBlock,
    Path,
    RelayTarget,
    Signal,
    Size,
    Strings,
    Url,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum RtmpRelayKind {
    Push,
    Pull,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum RtmpRuntimeStatus {
    ParsedNotEnforced,
    SourceNoOp,
    SourceBug,
    Deprecated,
    PlatformLimited,
}

#[test]
fn directive_manifests_are_versioned_and_cover_registered_forms() {
    let nginx: DirectiveManifest<DirectiveForm> = read_manifest("nginx-directives.json");
    let haproxy: DirectiveManifest<DirectiveForm> = read_manifest("haproxy-directives.json");
    let apache: DirectiveManifest<DirectiveForm> = read_manifest("apache-directives.json");
    let squid: SquidDirectiveManifest = read_manifest("squid-directives.json");

    validate_reference(
        &nginx,
        "nginx",
        "git_checkout",
        "eaac3d7",
        "docs/UPSTREAM_ANALYSIS.md",
    );
    validate_reference(
        &haproxy,
        "haproxy",
        "git_checkout",
        "ca686e3",
        "docs/UPSTREAM_ANALYSIS.md",
    );
    validate_reference(
        &apache,
        "apache",
        "git_checkout",
        "2.4.62",
        "docs/UPSTREAM_ANALYSIS.md",
    );
    validate_directive_forms(&nginx.entries);
    validate_directive_forms(&haproxy.entries);
    validate_directive_forms(&apache.entries);
    assert_eq!(squid.schema_version, 2);
    assert_eq!(squid.product, "squid");
    assert_eq!(squid.reference.kind, "git_checkout");
    assert_eq!(squid.reference.value, "6f4c814");
    assert_eq!(squid.reference.documentation, "docs/UPSTREAM_ANALYSIS.md");
    assert!(workspace_path(&squid.reference.documentation).is_file());
    assert_eq!(squid.registry_version, 2);
    assert_eq!(squid.target_version, "6f4c814");
    assert_eq!(squid.profile.id, "squid-forward-http1");
    assert_eq!(squid.profile.version, 2);
    assert_eq!(squid.parity, CapabilityStatus::Partial);
    assert!(!squid.complete_parity);
    assert!(!squid.audit.complete_parity_claim);
    assert!(!squid.families.is_empty());
    assert_unique_ids(
        "Squid family ID",
        squid.families.iter().map(|family| family.id.as_str()),
    );
    validate_squid_families(&squid.families);
    validate_squid_directive_forms(&squid.families, &squid.entries);
}

#[test]
fn nginx_rtmp_manifest_matches_all_registry_metadata() {
    let manifest: RtmpDirectiveManifest = read_manifest("nginx-rtmp-directives.json");
    validate_rtmp_reference(&manifest);

    let entries = manifest
        .entries
        .iter()
        .map(|entry| (entry.key.as_str(), entry))
        .collect::<HashMap<_, _>>();
    assert_eq!(
        entries.len(),
        manifest.entries.len(),
        "duplicate nginx-RTMP keys"
    );
    assert_eq!(entries.len(), directive_specs().len());

    for spec in directive_specs() {
        let entry = entries
            .get(spec.name)
            .unwrap_or_else(|| panic!("missing nginx-RTMP registry key `{}`", spec.name));
        assert_rtmp_metadata(entry, spec);
        let arguments = valid_rtmp_arguments(spec);
        for context in spec.contexts {
            assert!(
                validate_directive(spec.name, *context, &arguments).is_ok(),
                "registered nginx-RTMP directive `{}` rejected its authoritative valid probe in {context:?}",
                spec.name
            );
        }
        if let Some(context) = all_rtmp_contexts()
            .into_iter()
            .find(|context| !spec.contexts.contains(context))
        {
            assert!(matches!(
                validate_directive(spec.name, context, &arguments),
                Err(DirectiveError::InvalidContext { .. })
            ));
        }
    }
    for entry in &manifest.entries {
        validate_rtmp_terminal_decision(entry);
        validate_test_categories(&entry.id, &entry.required_tests);
    }
    validate_rtmp_import_forms(&manifest);
}

#[test]
fn all_manifest_ids_are_globally_unique() {
    let canonical: CanonicalManifest = read_manifest("canonical.json");
    let nginx: DirectiveManifest<DirectiveForm> = read_manifest("nginx-directives.json");
    let haproxy: DirectiveManifest<DirectiveForm> = read_manifest("haproxy-directives.json");
    let apache: DirectiveManifest<DirectiveForm> = read_manifest("apache-directives.json");
    let rtmp: RtmpDirectiveManifest = read_manifest("nginx-rtmp-directives.json");
    let squid: SquidDirectiveManifest = read_manifest("squid-directives.json");
    let components: ComponentManifest = read_manifest("components.json");
    let hosts: crate::host_ledger::HostManifest = read_manifest("host-cases.json");

    assert_unique_ids(
        "coverage ID",
        canonical
            .entries
            .iter()
            .map(|entry| entry.id.as_str())
            .chain(nginx.entries.iter().map(|entry| entry.id.as_str()))
            .chain(haproxy.entries.iter().map(|entry| entry.id.as_str()))
            .chain(apache.entries.iter().map(|entry| entry.id.as_str()))
            .chain(rtmp.entries.iter().map(|entry| entry.id.as_str()))
            .chain(rtmp.import_forms.iter().map(|entry| entry.id.as_str()))
            .chain(squid.entries.iter().map(|entry| entry.id.as_str()))
            .chain(components.entries.iter().map(|entry| entry.id.as_str()))
            .chain(hosts.cases.iter().map(|case| case.id.as_str())),
    );
}

#[test]
fn manifest_evidence_resolves_to_current_test_functions() {
    let canonical: CanonicalManifest = read_manifest("canonical.json");
    let nginx: DirectiveManifest<DirectiveForm> = read_manifest("nginx-directives.json");
    let haproxy: DirectiveManifest<DirectiveForm> = read_manifest("haproxy-directives.json");
    let apache: DirectiveManifest<DirectiveForm> = read_manifest("apache-directives.json");
    let rtmp: RtmpDirectiveManifest = read_manifest("nginx-rtmp-directives.json");
    let squid: SquidDirectiveManifest = read_manifest("squid-directives.json");
    let components: ComponentManifest = read_manifest("components.json");
    let hosts: crate::host_ledger::HostManifest = read_manifest("host-cases.json");
    let evidence: EvidenceManifest = read_manifest("evidence.json");
    assert_eq!(evidence.schema_version, 1);

    let manifest_references = [
        canonical.evidence,
        nginx.evidence,
        haproxy.evidence,
        apache.evidence,
        rtmp.evidence,
        squid.evidence,
        components.evidence,
        hosts.evidence,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let unique_ids = manifest_references.iter().cloned().collect::<BTreeSet<_>>();
    assert_eq!(
        unique_ids.len(),
        manifest_references.len(),
        "duplicate manifest evidence reference"
    );
    let registered = evidence
        .entries
        .iter()
        .map(|entry| entry.id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        registered.len(),
        evidence.entries.len(),
        "duplicate evidence ID"
    );
    assert_set_equality("manifest evidence IDs", &registered, &unique_ids);

    let mut test_references = HashSet::new();
    for entry in &evidence.entries {
        assert_nonempty_unique(&entry.tests, &entry.id, "test evidence references");
        for reference in &entry.tests {
            assert!(
                test_references.insert(reference),
                "duplicate test evidence reference `{reference}`"
            );
            assert_test_reference(entry, reference);
        }
    }
}

fn assert_test_reference(evidence: &EvidenceEntry, reference: &str) {
    let (relative_path, function_name) = reference_parts(&evidence.id, reference);
    assert!(
        relative_path.contains("/tests/")
            && Path::new(relative_path)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("rs")),
        "{} evidence must reference a Rust test file: {reference}",
        evidence.id
    );
    let source = read_source(relative_path);
    let syntax = syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("parse evidence {relative_path}: {error}"));
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
        "{} evidence does not resolve exactly one function: {reference}",
        evidence.id
    );
    assert!(
        matches[0].attrs.iter().any(is_test_attribute),
        "{} evidence is stale because the function is not a test: {reference}",
        evidence.id
    );
}

fn is_test_attribute(attribute: &Attribute) -> bool {
    attribute
        .path()
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "test")
}

fn validate_reference<E>(
    manifest: &DirectiveManifest<E>,
    product: &str,
    kind: &str,
    value: &str,
    documentation: &str,
) {
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.product, product);
    assert_eq!(manifest.reference.kind, kind);
    assert_eq!(manifest.reference.value, value);
    assert_eq!(manifest.reference.documentation, documentation);
    assert!(workspace_path(documentation).is_file());
}

fn validate_directive_forms(entries: &[DirectiveForm]) {
    for entry in entries {
        assert!(!entry.id.is_empty());
        assert!(!entry.key.is_empty(), "{} has an empty key", entry.id);
        assert!(!entry.form.is_empty(), "{} has an empty form", entry.id);
        assert_nonempty_unique(&entry.contexts, &entry.id, "contexts");
        validate_terminal_target(&entry.id, entry.disposition, &entry.target);
        validate_test_categories(&entry.id, &entry.required_tests);
    }
}

fn validate_squid_families(families: &[SquidFamilyManifest]) {
    for family in families {
        assert!(!family.id.is_empty());
        assert!(!family.name.is_empty(), "{} has an empty name", family.id);
        assert!(
            !family.rationale.is_empty(),
            "{} has no rationale",
            family.id
        );
        assert_nonempty_unique(
            &family.current_evidence,
            &family.id,
            "current evidence references",
        );
        validate_test_categories(&family.id, &family.required_tests);
    }
}

fn validate_squid_directive_forms(
    families: &[SquidFamilyManifest],
    entries: &[SquidDirectiveForm],
) {
    let family_ids = families
        .iter()
        .map(|family| family.id.as_str())
        .collect::<BTreeSet<_>>();
    for entry in entries {
        assert!(!entry.id.is_empty());
        assert!(!entry.key.is_empty(), "{} has an empty key", entry.id);
        assert!(!entry.family.is_empty(), "{} has no family", entry.id);
        assert!(!entry.form.is_empty(), "{} has an empty form", entry.id);
        assert!(
            family_ids.contains(entry.family.as_str()),
            "{} references an absent family {}",
            entry.id,
            entry.family
        );
        assert_nonempty_unique(&entry.contexts, &entry.id, "contexts");
        assert!(!entry.rationale.is_empty(), "{} has no rationale", entry.id);
        assert_nonempty_unique(
            &entry.current_evidence,
            &entry.id,
            "current evidence references",
        );
        validate_terminal_target(&entry.id, entry.disposition, &entry.target);
        validate_test_categories(&entry.id, &entry.required_tests);
        if entry.status == CapabilityStatus::Compatible {
            assert!(
                entry
                    .required_tests
                    .iter()
                    .any(|test| test == "integration"),
                "{} compatible form lacks runtime integration coverage",
                entry.id
            );
            assert!(
                entry.required_tests.iter().any(|test| test == "failure"),
                "{} compatible form lacks failure coverage",
                entry.id
            );
            assert!(
                matches!(
                    entry.disposition,
                    Disposition::Lowered | Disposition::Classified
                ),
                "{} compatible form is not lowered or classified",
                entry.id
            );
        }
        if matches!(
            entry.status,
            CapabilityStatus::Unsupported
                | CapabilityStatus::Obsolete
                | CapabilityStatus::NotPlanned
        ) {
            assert!(
                matches!(
                    entry.disposition,
                    Disposition::Blocked | Disposition::Externalized
                ),
                "{} open form is not blocked or externalized",
                entry.id
            );
        }
    }
}

fn assert_rtmp_metadata(entry: &RtmpDirectiveEntry, spec: &DirectiveSpec) {
    assert_eq!(
        entry.id,
        format!("directive.nginx-rtmp.{}", spec.name.replace('_', "-"))
    );
    assert_eq!(entry.contexts, rtmp_contexts(spec.contexts));
    assert_eq!(entry.min_args, spec.min_args);
    assert_eq!(entry.max_args, spec.max_args);
    let (value_kind, values, relay_kind) = rtmp_value_kind(spec.value_kind);
    assert_eq!(entry.value_kind, value_kind, "{} value kind", spec.name);
    assert_eq!(entry.values, values, "{} closed values", spec.name);
    assert_eq!(entry.relay_kind, relay_kind, "{} relay kind", spec.name);
    assert_eq!(entry.default.as_deref(), spec.default);
    assert_eq!(entry.repeatable, spec.repeatable);
    assert_eq!(
        entry.runtime_status,
        rtmp_runtime_status(spec.runtime_support)
    );
}

fn validate_rtmp_terminal_decision(entry: &RtmpDirectiveEntry) {
    let (disposition, target) = match entry.runtime_status {
        RtmpRuntimeStatus::SourceBug => (Disposition::Blocked, "import.blocking_diagnostic"),
        RtmpRuntimeStatus::PlatformLimited => (Disposition::Externalized, "deployment"),
        RtmpRuntimeStatus::ParsedNotEnforced
        | RtmpRuntimeStatus::SourceNoOp
        | RtmpRuntimeStatus::Deprecated => (Disposition::Classified, "rtmp.directive_registry"),
    };
    assert_eq!(entry.disposition, disposition, "{} disposition", entry.id);
    assert_eq!(entry.target, target, "{} target", entry.id);
    validate_terminal_target(&entry.id, entry.disposition, &entry.target);
}

fn validate_rtmp_reference(manifest: &RtmpDirectiveManifest) {
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.product, "nginx-rtmp");
    assert_eq!(manifest.reference.kind, "release_and_git_checkout");
    assert_eq!(manifest.reference.value, "1.1.4+git.6c7719d");
    assert_eq!(manifest.reference.documentation, "docs/RTMP_SPEC.md");
    assert!(workspace_path(&manifest.reference.documentation).is_file());
}

fn validate_rtmp_import_forms(manifest: &RtmpDirectiveManifest) {
    let expected = [
        "import.nginx-rtmp.allow.bounded",
        "import.nginx-rtmp.deny.bounded",
        "import.nginx-rtmp.max-connections.application",
        "import.nginx-rtmp.max-message.bounded",
        "import.nginx-rtmp.ack-window.bounded",
        "import.nginx-rtmp.exec-publisher.typed",
        "import.nginx-rtmp.exec-alias.typed",
        "import.nginx-rtmp.exec-publish.typed",
        "import.nginx-rtmp.exec-publish-done.typed",
        "import.nginx-rtmp.respawn.bounded",
        "import.nginx-rtmp.record.off",
        "import.nginx-rtmp.record.all",
        "import.nginx-rtmp.record.all-manual",
        "import.nginx-rtmp.record.inexact",
        "import.nginx-rtmp.record-path.secure-absolute",
        "import.nginx-rtmp.record-path.invalid",
        "import.nginx-rtmp.record-suffix.exact",
        "import.nginx-rtmp.record-suffix.inexact",
        "import.nginx-rtmp.record-unique.flag",
        "import.nginx-rtmp.record-interval.exact",
        "import.nginx-rtmp.record-interval.invalid-or-manual",
        "import.nginx-rtmp.record-append.disabled",
        "import.nginx-rtmp.record-append.enabled",
        "import.nginx-rtmp.record-lock.disabled",
        "import.nginx-rtmp.record-lock.enabled",
        "import.nginx-rtmp.record-max-size.unlimited",
        "import.nginx-rtmp.record-max-size.limited",
        "import.nginx-rtmp.record-max-frames.unlimited",
        "import.nginx-rtmp.record-max-frames.limited",
        "import.nginx-rtmp.record-notify.disabled",
        "import.nginx-rtmp.record-notify.enabled",
        "import.nginx-rtmp.recorder.named",
        "import.nginx-rtmp.recorder.invalid",
        "import.nginx-rtmp.hls-muxdelay.source-noop",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    let actual = manifest
        .import_forms
        .iter()
        .map(|form| form.id.clone())
        .collect::<BTreeSet<_>>();
    assert_set_equality("nginx-RTMP importer forms", &expected, &actual);
    assert_eq!(
        actual.len(),
        manifest.import_forms.len(),
        "duplicate nginx-RTMP importer form"
    );

    let registry_keys = manifest
        .entries
        .iter()
        .map(|entry| entry.key.as_str())
        .collect::<HashSet<_>>();
    for form in &manifest.import_forms {
        assert!(!form.form.is_empty(), "{} has an empty form", form.id);
        assert!(
            registry_keys.contains(form.key.as_str()),
            "{} references unregistered directive `{}`",
            form.id,
            form.key
        );
        assert!(!form.contexts.is_empty(), "{} has no contexts", form.id);
        assert_eq!(
            form.contexts.iter().collect::<HashSet<_>>().len(),
            form.contexts.len(),
            "{} repeats importer contexts",
            form.id
        );
        assert!(
            matches!(
                form.disposition,
                Disposition::Lowered | Disposition::Classified | Disposition::Blocked
            ),
            "{} must describe an enforced importer decision",
            form.id
        );
        validate_terminal_target(&form.id, form.disposition, &form.target);
        validate_test_categories(&form.id, &form.required_tests);
    }

    for key in [
        "record",
        "record_path",
        "record_suffix",
        "record_unique",
        "record_append",
        "record_lock",
        "record_max_size",
        "record_max_frames",
        "record_interval",
        "record_notify",
        "recorder",
        "hls_muxdelay",
    ] {
        assert!(
            manifest.import_forms.iter().any(|form| form.key == key),
            "recording directive `{key}` has no strict importer disposition"
        );
    }
}

fn rtmp_contexts(contexts: &[DirectiveContext]) -> Vec<RtmpContext> {
    contexts
        .iter()
        .map(|context| match context {
            DirectiveContext::NginxMain => RtmpContext::NginxMain,
            DirectiveContext::RtmpMain => RtmpContext::RtmpMain,
            DirectiveContext::RtmpServer => RtmpContext::RtmpServer,
            DirectiveContext::RtmpApplication => RtmpContext::RtmpApplication,
            DirectiveContext::RtmpRecorder => RtmpContext::RtmpRecorder,
            DirectiveContext::Http => RtmpContext::Http,
        })
        .collect()
}

fn rtmp_value_kind(kind: ValueKind) -> (RtmpValueKind, Vec<String>, Option<RtmpRelayKind>) {
    let (kind, values, relay) = match kind {
        ValueKind::AccessLog => (RtmpValueKind::AccessLog, &[][..], None),
        ValueKind::AccessRule => (RtmpValueKind::AccessRule, &[][..], None),
        ValueKind::Bitmask(values) => (RtmpValueKind::Bitmask, values, None),
        ValueKind::Block => (RtmpValueKind::Block, &[][..], None),
        ValueKind::Command => (RtmpValueKind::Command, &[][..], None),
        ValueKind::Duration => (RtmpValueKind::Duration, &[][..], None),
        ValueKind::DurationOrOff => (RtmpValueKind::DurationOrOff, &[][..], None),
        ValueKind::Enum(values) => (RtmpValueKind::Enum, values, None),
        ValueKind::Flag => (RtmpValueKind::Flag, &[][..], None),
        ValueKind::HlsVariant => (RtmpValueKind::HlsVariant, &[][..], None),
        ValueKind::Integer => (RtmpValueKind::Integer, &[][..], None),
        ValueKind::Listen => (RtmpValueKind::Listen, &[][..], None),
        ValueKind::LogFormat => (RtmpValueKind::LogFormat, &[][..], None),
        ValueKind::NamedBlock => (RtmpValueKind::NamedBlock, &[][..], None),
        ValueKind::Path => (RtmpValueKind::Path, &[][..], None),
        ValueKind::RelayTarget(RelayKind::Push) => (
            RtmpValueKind::RelayTarget,
            &[][..],
            Some(RtmpRelayKind::Push),
        ),
        ValueKind::RelayTarget(RelayKind::Pull) => (
            RtmpValueKind::RelayTarget,
            &[][..],
            Some(RtmpRelayKind::Pull),
        ),
        ValueKind::Signal => (RtmpValueKind::Signal, &[][..], None),
        ValueKind::Size => (RtmpValueKind::Size, &[][..], None),
        ValueKind::Strings => (RtmpValueKind::Strings, &[][..], None),
        ValueKind::Url => (RtmpValueKind::Url, &[][..], None),
    };
    (
        kind,
        values.iter().map(ToString::to_string).collect(),
        relay,
    )
}

const fn rtmp_runtime_status(status: RuntimeSupport) -> RtmpRuntimeStatus {
    match status {
        RuntimeSupport::ParsedNotEnforced => RtmpRuntimeStatus::ParsedNotEnforced,
        RuntimeSupport::SourceNoOp => RtmpRuntimeStatus::SourceNoOp,
        RuntimeSupport::SourceBug => RtmpRuntimeStatus::SourceBug,
        RuntimeSupport::Deprecated => RtmpRuntimeStatus::Deprecated,
        RuntimeSupport::PlatformLimited => RtmpRuntimeStatus::PlatformLimited,
    }
}

fn valid_rtmp_arguments(spec: &DirectiveSpec) -> Vec<&'static str> {
    let first = match spec.value_kind {
        ValueKind::AccessLog => "off",
        ValueKind::AccessRule => "all",
        ValueKind::Bitmask(values) | ValueKind::Enum(values) => values[0],
        ValueKind::Block => return Vec::new(),
        ValueKind::Command => "true",
        ValueKind::Duration | ValueKind::DurationOrOff => "1s",
        ValueKind::Flag => "on",
        ValueKind::HlsVariant => "low",
        ValueKind::Integer => "1",
        ValueKind::Listen => "1935",
        ValueKind::LogFormat => "main",
        ValueKind::NamedBlock => "name",
        ValueKind::Path => "/tmp/value",
        ValueKind::RelayTarget(_) => "rtmp://origin/live",
        ValueKind::Signal => "TERM",
        ValueKind::Size => "1M",
        ValueKind::Strings => "value",
        ValueKind::Url => "http://localhost/hook",
    };
    let mut arguments = vec![first; usize::from(spec.min_args)];
    if spec.value_kind == ValueKind::LogFormat && arguments.len() >= 2 {
        arguments[1] = "format";
    }
    arguments
}

const fn all_rtmp_contexts() -> [DirectiveContext; 6] {
    [
        DirectiveContext::NginxMain,
        DirectiveContext::RtmpMain,
        DirectiveContext::RtmpServer,
        DirectiveContext::RtmpApplication,
        DirectiveContext::RtmpRecorder,
        DirectiveContext::Http,
    ]
}

pub(crate) fn validate_runtime_decision(id: &str, decision: &RuntimeDecision) {
    validate_terminal_target(id, decision.disposition, &decision.target);
    assert!(
        matches!(
            decision.disposition,
            Disposition::Lowered | Disposition::Classified
        ),
        "canonical entry `{id}` cannot be externalized or blocked after acceptance"
    );
}

pub(crate) fn validate_terminal_target(id: &str, disposition: Disposition, target: &str) {
    assert!(
        TARGETS.contains(&target) || ADDITIONAL_TARGETS.contains(&target),
        "coverage entry `{id}` has invalid target `{target}`"
    );
    match disposition {
        Disposition::Externalized => assert_eq!(target, "deployment", "{id}"),
        Disposition::Blocked => assert_eq!(target, "import.blocking_diagnostic", "{id}"),
        Disposition::Lowered => assert_ne!(target, "deployment", "{id}"),
        Disposition::Classified => assert_ne!(target, "import.blocking_diagnostic", "{id}"),
    }
}

pub(crate) fn validate_test_categories(id: &str, categories: &[String]) {
    assert_nonempty_unique(categories, id, "required test categories");
    for category in categories {
        assert!(
            TEST_CATEGORIES.contains(&category.as_str()),
            "coverage entry `{id}` has invalid test category `{category}`"
        );
    }
}
