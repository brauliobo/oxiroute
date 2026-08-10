use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use oxiroute_config::{HttpHostSelector, UpstreamEndpoint};

use crate::{
    Diagnostic, DiagnosticCode, DiagnosticStage, E_DUPLICATE_IDENTITY, E_INVALID_VALUE,
    E_SEMANTICS_NOT_REPRESENTABLE, E_UNRESOLVED_REFERENCE, E_UNSUPPORTED_FEATURE, Report, Severity,
    Span, canonical::dns_name,
};

use super::{
    E_AMBIGUOUS_VHOST, E_DIRECTORY_MERGE, E_DYNAMIC_BALANCER_MANAGER, E_DYNAMIC_PROXY_PASS,
    E_REWRITE_UNSUPPORTED, E_UNSUPPORTED_DIRECTIVE, E_UNSUPPORTED_MODULE, ExpandedDirective,
    IncludeFrame, OccurrenceId, Provenance, SourceGraph, Word,
};

include!("semantic/model.rs");
include!("semantic/resolver.rs");
