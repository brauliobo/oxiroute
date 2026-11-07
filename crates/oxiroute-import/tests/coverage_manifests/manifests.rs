use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::Path,
};

use oxiroute_import::nginx::{
    DirectiveContext, DirectiveError, DirectiveSpec, RelayKind, RuntimeSupport, ValueKind,
    directive_specs, validate_directive,
};
use serde::Deserialize;
use syn::{Attribute, Item};

use crate::support::{
    assert_nonempty_unique, assert_set_equality, assert_unique_ids, read_manifest, read_source,
    reference_parts, workspace_path,
};

#[path = "manifests/checks.rs"]
mod checks;
#[path = "manifests/validators.rs"]
mod validators;

#[allow(clippy::wildcard_imports)]
pub(crate) use validators::*;

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
