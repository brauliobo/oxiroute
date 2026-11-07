use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    net::{IpAddr, Ipv6Addr, SocketAddr},
    os::unix::ffi::{OsStrExt as _, OsStringExt as _},
    path::{Path, PathBuf},
};

use oxiroute_config::{
    ConfigDraft, DownstreamTimeoutPolicy, ForwardAccessAction, ForwardAccessCondition,
    ForwardAccessMatcher, ForwardAccessPolicy, ForwardAccessRule, ForwardAuditMode,
    ForwardConnectPolicy, ForwardDestinationPolicy, ForwardDirectFallback, ForwardHeaderPolicy,
    ForwardHttpVersion, ForwardPeer, ForwardPeerPolicy, ForwardPortRange, ForwardProxyAuth,
    ForwardProxyService, ForwardResolverPolicy, ForwardViaPolicy, ForwardedForPolicy, Listener,
    ListenerBind, Protocol,
};

use crate::canonical::{dns_name, ip_address};
use crate::{
    CanonicalCandidate, CanonicalProvenance, Diagnostic, DiagnosticCode, DiagnosticStage,
    E_DUPLICATE_IDENTITY, E_SEMANTICS_NOT_REPRESENTABLE, E_UNRESOLVED_REFERENCE,
    E_UNSUPPORTED_FEATURE, Severity, SourceImportMetadata,
    candidate::{
        CanonicalCandidateState, CanonicalProvenanceLedger, EmptyOriginPolicy, empty_config,
        finalize_candidate,
    },
};

use super::{
    AccessAction, AccessListKind, AclMatcher, AclReferenceResolution, AclTerm, Activation,
    AuthenticationScheme, AuthenticationSetting, BuiltinAcl, DecisionLedger, DecisionOutcome,
    DirectiveOrigin, EffectiveAcl, EffectiveConfiguration, ForwardedForMode, LogDestination,
    OccurrenceId, PortEndpoint, PortKind, PrivacyDirective, ProxyAuthMatcher, RootSelection,
    SemanticBlockerKind, SourceGraph, SquidCapabilityReport, analyze, load, load_selected,
    squid_capability_report,
};

mod blockers;
mod consumed;
mod lowering;
mod provenance;
mod report;

use blockers::{blocker_code, blocker_message, is_schema_blocker, lower_blocker_diagnostic};
use consumed::consumed_occurrences;
use lowering::{lower, lower_access_policy, lower_authentication, lower_peer_policy};
use provenance::lower_provenance;
pub use report::*;
