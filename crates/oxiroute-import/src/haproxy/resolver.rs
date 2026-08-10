use std::{
    collections::{HashMap, HashSet},
    fs::{File, Metadata},
    io::{BufReader, Read},
    path::PathBuf,
    time::{Duration, SystemTime},
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use openssl::{
    pkey::{Id, PKey},
    x509::X509,
};
use oxiroute_config::{PassiveObserve, PassiveOnError, canonical_certificate_dns_name};
use rustls_pki_types::pem::{self, SectionKind as PemSectionKind};
use x509_parser::{extensions::GeneralName, parse_x509_certificate};
use zeroize::Zeroizing;

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

mod http_directives;
mod server;

use http_directives::{
    parse_acl, parse_forward_for, parse_http_check, parse_http_check_send, parse_http_request_rule,
    parse_http_response_rule, parse_status_ranges,
};
use server::{merge_server_defaults, parse_server};

include!("resolver/effective.rs");
include!("resolver/engine.rs");
include!("resolver/directives.rs");
include!("resolver/certificates.rs");
include!("resolver/support.rs");
