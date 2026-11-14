use std::{
    collections::{HashMap, HashSet},
    io::BufReader,
    path::PathBuf,
    time::Duration,
};

use openssl::{
    pkey::{Id, PKey},
    x509::X509,
};
use oxiroute_config::{PassiveObserve, PassiveOnError};
use rustls_pki_types::pem::{self, SectionKind as PemSectionKind};
use zeroize::Zeroizing;

use crate::certificate::{PemMetadata, certificate_metadata, read_pem_items, read_stable_pem};
use crate::{
    ActivationRequirement, ActivationRequirementKind, DeploymentRequirement,
    DeploymentRequirementKind, Diagnostic, DiagnosticCode, DiagnosticStage, ProvenanceRole,
    ProvenanceSpan, Report, Severity, SourceId, Span,
};
pub use crate::{E_DUPLICATE_IDENTITY, E_UNRESOLVED_REFERENCE};

use super::{
    Configuration, Directive, E_CONDITIONAL_PREPROCESSING, E_ENVIRONMENT_EXPANSION, Section,
    SectionKind,
};

mod engine;
mod http_directives;
mod server;

pub(super) use engine::resolve_report;
use engine::{ParsedHeader, PendingAclCondition, SectionMeta};
use http_directives::{
    parse_acl, parse_forward_for, parse_http_check, parse_http_check_send, parse_http_request_rule,
    parse_http_response_rule, parse_status_ranges,
};
use server::{merge_server_defaults, parse_server};

include!("resolver/effective.rs");
include!("resolver/directives.rs");
include!("resolver/certificates.rs");
include!("resolver/support.rs");
