use std::{fmt::Write as _, path::Path};

use openssl::sha::sha256;
use serde::Serialize;

use crate::{
    ActivationRequirement, CanonicalCandidate, CanonicalCandidateSummary, CanonicalProvenance,
    DeploymentRequirement, Diagnostic, OperationalOverlayRequirement, Report, Severity, SourceFile,
    SourceId, SourceImportMetadata, Span,
};

mod blockers;
mod envelope;
mod graph;
mod model;
mod source;

use blockers::{
    apache_blockers, diagnostic_blockers, diagnostic_evidence, nginx_blockers, snake_case,
    sorted_diagnostics, squid_blockers, varnish_blockers,
};

use graph::{apache_graph, haproxy_graph, nginx_graph, squid_graph, varnish_graph};
pub use model::*;
use source::{
    apache_origin, bytes_string, graph_source_metadata, haproxy_origin, nginx_origin, path_string,
    source_fingerprint, source_metadata_evidence, source_reference, source_roots, span_evidence,
    squid_origin, varnish_origin,
};
