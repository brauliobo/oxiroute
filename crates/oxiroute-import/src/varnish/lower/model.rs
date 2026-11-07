#[allow(clippy::wildcard_imports)]
use super::*;

/// Stable capability profile used by Varnish reports and native source references.
pub const VARNISH_CAPABILITY_PROFILE_ID: &str = "varnish-vcl-exact-cache";
pub const VARNISH_CAPABILITY_PROFILE_VERSION: u32 = 1;

/// Canonical candidate produced by the exact Varnish lowering pass.
pub type VarnishCanonicalCandidate = CanonicalCandidate<Provenance>;

pub(super) const DEFAULT_LISTEN: SocketAddr =
    SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 6081);
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 3_500;
const DEFAULT_FIRST_BYTE_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_BETWEEN_BYTES_TIMEOUT_MS: u64 = 60_000;
pub(super) const DEFAULT_TTL_MS: u64 = 120_000;
pub(super) const DEFAULT_GRACE_MS: u64 = 10_000;
pub(super) const DEFAULT_KEEP_MS: u64 = 0;
pub(super) const DEFAULT_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy)]
#[allow(clippy::struct_field_names)]
pub(super) struct RouteTiming {
    pub(super) connect_ms: u64,
    pub(super) read_ms: u64,
    pub(super) write_ms: u64,
}

impl Default for RouteTiming {
    fn default() -> Self {
        Self {
            connect_ms: DEFAULT_CONNECT_TIMEOUT_MS,
            read_ms: DEFAULT_FIRST_BYTE_TIMEOUT_MS,
            write_ms: DEFAULT_BETWEEN_BYTES_TIMEOUT_MS,
        }
    }
}

#[derive(Clone)]
pub(super) struct BackendInfo {
    pub(super) pool: UpstreamPool,
    pub(super) timing: RouteTiming,
    pub(super) health: Option<HealthProbeInfo>,
}

#[derive(Clone)]
pub(super) struct DirectorInfo {
    pub(super) pool: UpstreamPool,
    pub(super) timing: RouteTiming,
    pub(super) health: Option<HealthProbeInfo>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct HealthProbeInfo {
    pub(super) check: HealthCheck,
    pub(super) provenance: Vec<(String, Vec<Provenance>)>,
}

impl HealthProbeInfo {
    pub(super) fn merge_provenance(&mut self, other: &Self) {
        for (path, origins) in &other.provenance {
            let Some((_, existing)) = self.provenance.iter_mut().find(|(known, _)| known == path)
            else {
                self.provenance.push((path.clone(), origins.clone()));
                continue;
            };
            for origin in origins {
                if !existing.contains(origin) {
                    existing.push(origin.clone());
                }
            }
        }
    }
}

pub(super) struct ProbeError {
    pub(super) provenance: Provenance,
    pub(super) message: &'static str,
}

#[derive(Clone)]
pub(super) struct InvocationPolicy {
    pub(super) listeners: Vec<SocketAddr>,
    pub(super) storage: Option<CacheStore>,
    pub(super) default_ttl_ms: u64,
    pub(super) default_grace_ms: u64,
    pub(super) default_keep_ms: u64,
}

pub(super) struct Lowerer<'a> {
    pub(super) graph: &'a SourceGraph,
    pub(super) source_invocation: &'a InvocationFacts,
    pub(super) backends: &'a [Backend],
    pub(super) directors: &'a [Director],
    pub(super) probes: &'a [Probe],
    pub(super) modern_directors: &'a [ModernDirector],
    pub(super) subroutines: &'a [Subroutine],
    pub(super) statements: &'a [StatementDecision],
    pub(super) imports: &'a [VmodImport],
    pub(super) vmod_objects: &'a [VmodObject],
    pub(super) diagnostics: Vec<Diagnostic>,
    pub(super) blocker: Option<LoweringBlocker>,
    pub(super) draft: ConfigDraft,
    pub(super) provenance: CanonicalProvenanceLedger<Provenance>,
    pub(super) backend_infos: BTreeMap<usize, BackendInfo>,
    pub(super) director_infos: BTreeMap<usize, DirectorInfo>,
    pub(super) invocation_policy: Option<InvocationPolicy>,
    pub(super) root_origin: Option<Provenance>,
}
