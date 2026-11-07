use std::{collections::BTreeSet, fmt::Write as _};

use oxiroute_config::{
    HttpHostSelector, HttpPathSelector, ListenerBind, UpstreamAlgorithm, UpstreamEndpoint,
};
use oxiroute_import::{
    DiagnosticStage, Report,
    haproxy::{
        BlockingReason, CanonicalCandidate as HaproxyCandidate, Configuration, DecisionOutcome,
        E_UNCONSUMED_DIRECTIVE, E_UNKNOWN_DIRECTIVE, EffectiveConfiguration, SectionKind,
        import_parsed as import_haproxy, resolve_parsed,
    },
};
use syn::Item;

use crate::{
    manifests::{DirectiveForm, DirectiveManifest, Disposition},
    report_invariants::{
        HaproxyProbeReports, assert_haproxy_report_invariants, parse_haproxy_source,
    },
    support::{assert_set_equality, read_manifest, read_source},
};

#[path = "haproxy_probes/contexts.rs"]
mod contexts;
#[path = "haproxy_probes/probes.rs"]
mod probes;
#[path = "haproxy_probes/tests.rs"]
mod tests;

#[allow(clippy::wildcard_imports)]
use contexts::*;
#[allow(clippy::wildcard_imports)]
use probes::*;
