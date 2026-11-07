#[allow(clippy::wildcard_imports)]
use super::*;

struct SourceDependencyAdapter {
    source_id: u32,
    target_source_id: Option<u32>,
    requested_path: Option<String>,
    canonical_path: Option<String>,
    optional: Option<bool>,
    status: &'static str,
    span: Option<SpanEvidence>,
    failure_code: Option<String>,
    truncated: bool,
}

struct SourceGraphEvidenceBuilder {
    roots: Vec<SourceRootEvidence>,
    sources: Vec<SourceReference>,
    dependencies: Vec<DependencyEvidence>,
    dependencies_complete: bool,
    snapshot_stable: Option<bool>,
}

impl SourceGraphEvidenceBuilder {
    fn new(
        root: Option<SourceId>,
        sources: Vec<SourceReference>,
        dependencies_complete: bool,
        snapshot_stable: Option<bool>,
    ) -> Self {
        Self {
            roots: source_roots(root, &sources),
            sources,
            dependencies: Vec::new(),
            dependencies_complete,
            snapshot_stable,
        }
    }

    fn dependency(&mut self, adapter: SourceDependencyAdapter) {
        self.dependencies.push(DependencyEvidence {
            source_id: adapter.source_id,
            target_source_id: adapter.target_source_id,
            kind: "include".into(),
            requested_path: adapter.requested_path,
            canonical_path: adapter.canonical_path,
            optional: adapter.optional,
            status: adapter.status.into(),
            span: adapter.span,
            failure_code: adapter.failure_code,
            fingerprint_sha256: source_fingerprint(&self.sources, adapter.target_source_id),
            truncated: adapter.truncated,
        });
    }

    fn finish(self) -> SourceGraphEvidence {
        SourceGraphEvidence {
            roots: self.roots,
            sources: self.sources,
            dependencies: self.dependencies,
            dependencies_complete: self.dependencies_complete,
            snapshot_stable: self.snapshot_stable,
        }
    }
}

pub(super) fn nginx_graph(graph: &crate::nginx::SourceGraph) -> SourceGraphEvidence {
    let sources = graph
        .sources
        .iter()
        .map(|source| source_reference(&source.source, Some(&source.canonical_path)))
        .collect::<Vec<_>>();
    let mut builder =
        SourceGraphEvidenceBuilder::new(graph.root, sources, true, Some(graph.snapshot_stable));
    for edge in &graph.includes {
        if edge.candidates.is_empty() {
            for target in &edge.targets {
                builder.dependency(SourceDependencyAdapter {
                    source_id: edge.source.get(),
                    target_source_id: Some(target.get()),
                    requested_path: bytes_string(&edge.pattern),
                    canonical_path: None,
                    optional: Some(false),
                    status: "expanded",
                    span: Some(span_evidence(edge.span)),
                    failure_code: edge.failure.map(|code| code.as_str().into()),
                    truncated: edge.truncated,
                });
            }
            if edge.targets.is_empty() {
                builder.dependency(SourceDependencyAdapter {
                    source_id: edge.source.get(),
                    target_source_id: None,
                    requested_path: bytes_string(&edge.pattern),
                    canonical_path: None,
                    optional: Some(false),
                    status: "failed",
                    span: Some(span_evidence(edge.span)),
                    failure_code: edge.failure.map(|code| code.as_str().into()),
                    truncated: edge.truncated,
                });
            }
            continue;
        }
        for candidate in &edge.candidates {
            let (target_source_id, status) = nginx_target_status(candidate.status);
            builder.dependency(SourceDependencyAdapter {
                source_id: edge.source.get(),
                target_source_id,
                requested_path: path_string(&candidate.path),
                canonical_path: candidate.canonical_path.as_deref().and_then(path_string),
                optional: Some(false),
                status,
                span: Some(span_evidence(edge.span)),
                failure_code: edge.failure.map(|code| code.as_str().into()),
                truncated: edge.truncated,
            });
        }
    }
    builder.finish()
}

fn nginx_target_status(
    status: crate::nginx::IncludeCandidateStatus,
) -> (Option<u32>, &'static str) {
    match status {
        crate::nginx::IncludeCandidateStatus::Expanded(source)
        | crate::nginx::IncludeCandidateStatus::Cycle(source)
        | crate::nginx::IncludeCandidateStatus::ExpansionLimit(source) => {
            let status = match status {
                crate::nginx::IncludeCandidateStatus::Expanded(_) => "expanded",
                crate::nginx::IncludeCandidateStatus::Cycle(_) => "cycle",
                crate::nginx::IncludeCandidateStatus::ExpansionLimit(_) => "expansion_limit",
                _ => unreachable!(),
            };
            (Some(source.get()), status)
        }
        crate::nginx::IncludeCandidateStatus::CanonicalizeFailed => (None, "canonicalize_failed"),
        crate::nginx::IncludeCandidateStatus::SourceIo => (None, "source_io"),
        crate::nginx::IncludeCandidateStatus::SourceChanged => (None, "source_changed"),
        crate::nginx::IncludeCandidateStatus::SourceSizeLimit => (None, "source_size_limit"),
        crate::nginx::IncludeCandidateStatus::SourceFileLimit => (None, "source_file_limit"),
        crate::nginx::IncludeCandidateStatus::AggregateSourceLimit => {
            (None, "aggregate_source_limit")
        }
    }
}

pub(super) fn apache_graph(graph: &crate::apache::SourceGraph) -> SourceGraphEvidence {
    let sources = graph
        .sources
        .iter()
        .map(|source| source_reference(&source.source, Some(&source.canonical_path)))
        .collect::<Vec<_>>();
    let mut builder =
        SourceGraphEvidenceBuilder::new(graph.root, sources, true, Some(graph.snapshot_stable));
    for edge in &graph.includes {
        if edge.candidates.is_empty() {
            for target in &edge.targets {
                builder.dependency(SourceDependencyAdapter {
                    source_id: edge.source.get(),
                    target_source_id: Some(target.get()),
                    requested_path: bytes_string(&edge.pattern),
                    canonical_path: None,
                    optional: Some(edge.optional),
                    status: "expanded",
                    span: Some(span_evidence(edge.span)),
                    failure_code: edge.failure.map(|code| code.as_str().into()),
                    truncated: edge.truncated,
                });
            }
            if edge.targets.is_empty() {
                builder.dependency(SourceDependencyAdapter {
                    source_id: edge.source.get(),
                    target_source_id: None,
                    requested_path: bytes_string(&edge.pattern),
                    canonical_path: None,
                    optional: Some(edge.optional),
                    status: if edge.optional && edge.failure.is_none() {
                        "optional_missing"
                    } else {
                        "failed"
                    },
                    span: Some(span_evidence(edge.span)),
                    failure_code: edge.failure.map(|code| code.as_str().into()),
                    truncated: edge.truncated,
                });
            }
            continue;
        }
        for candidate in &edge.candidates {
            let (target_source_id, status) = apache_target_status(candidate.status);
            builder.dependency(SourceDependencyAdapter {
                source_id: edge.source.get(),
                target_source_id,
                requested_path: path_string(&candidate.path),
                canonical_path: candidate.canonical_path.as_deref().and_then(path_string),
                optional: Some(edge.optional),
                status,
                span: Some(span_evidence(edge.span)),
                failure_code: edge.failure.map(|code| code.as_str().into()),
                truncated: edge.truncated,
            });
        }
    }
    builder.finish()
}

fn apache_target_status(
    status: crate::apache::IncludeCandidateStatus,
) -> (Option<u32>, &'static str) {
    match status {
        crate::apache::IncludeCandidateStatus::Expanded(source) => (Some(source.get()), "expanded"),
        crate::apache::IncludeCandidateStatus::Cycle(source) => (Some(source.get()), "cycle"),
        crate::apache::IncludeCandidateStatus::CanonicalizeFailed => (None, "canonicalize_failed"),
        crate::apache::IncludeCandidateStatus::SourceIo => (None, "source_io"),
        crate::apache::IncludeCandidateStatus::SourceChanged => (None, "source_changed"),
        crate::apache::IncludeCandidateStatus::SourceSizeLimit => (None, "source_size_limit"),
        crate::apache::IncludeCandidateStatus::SourceFileLimit => (None, "source_file_limit"),
        crate::apache::IncludeCandidateStatus::AggregateSourceLimit => {
            (None, "aggregate_source_limit")
        }
    }
}

pub(super) fn varnish_graph(graph: &crate::varnish::SourceGraph) -> SourceGraphEvidence {
    let sources = graph
        .sources
        .iter()
        .map(|source| source_reference(&source.source, source.canonical_path.as_deref()))
        .collect::<Vec<_>>();
    let mut builder =
        SourceGraphEvidenceBuilder::new(graph.root, sources, true, Some(graph.snapshot_stable));
    for edge in &graph.includes {
        if edge.targets.is_empty() {
            builder.dependency(SourceDependencyAdapter {
                source_id: edge.source.get(),
                target_source_id: None,
                requested_path: bytes_string(&edge.pattern),
                canonical_path: None,
                optional: Some(false),
                status: "failed",
                span: Some(span_evidence(edge.span)),
                failure_code: edge.failure.map(|code| code.as_str().into()),
                truncated: edge.truncated,
            });
            continue;
        }
        for target in &edge.targets {
            let (target_source_id, status) = varnish_target_status(target.status);
            builder.dependency(SourceDependencyAdapter {
                source_id: edge.source.get(),
                target_source_id,
                requested_path: path_string(&target.requested_path),
                canonical_path: target.canonical_path.as_deref().and_then(path_string),
                optional: Some(false),
                status,
                span: Some(span_evidence(edge.span)),
                failure_code: edge.failure.map(|code| code.as_str().into()),
                truncated: edge.truncated,
            });
        }
    }
    builder.finish()
}

fn varnish_target_status(
    status: crate::varnish::IncludeTargetStatus,
) -> (Option<u32>, &'static str) {
    match status {
        crate::varnish::IncludeTargetStatus::Expanded(source) => (Some(source.get()), "expanded"),
        crate::varnish::IncludeTargetStatus::Cycle(source) => (Some(source.get()), "cycle"),
        crate::varnish::IncludeTargetStatus::SourceIo => (None, "source_io"),
        crate::varnish::IncludeTargetStatus::SourceChanged => (None, "source_changed"),
        crate::varnish::IncludeTargetStatus::SourceSizeLimit => (None, "source_size_limit"),
        crate::varnish::IncludeTargetStatus::SourceFileLimit => (None, "source_file_limit"),
        crate::varnish::IncludeTargetStatus::AggregateSourceLimit => {
            (None, "aggregate_source_limit")
        }
        crate::varnish::IncludeTargetStatus::ExpansionLimit => (None, "expansion_limit"),
    }
}

#[cfg(unix)]
pub(super) fn squid_graph(graph: &crate::squid::SourceGraph) -> SourceGraphEvidence {
    let sources = graph
        .sources
        .iter()
        .map(|source| source_reference(&source.source, Some(&source.canonical_path)))
        .collect::<Vec<_>>();
    let mut builder =
        SourceGraphEvidenceBuilder::new(graph.root, sources, true, Some(graph.snapshot_stable));
    for edge in &graph.includes {
        if edge.targets.is_empty() {
            builder.dependency(SourceDependencyAdapter {
                source_id: edge.source.get(),
                target_source_id: None,
                requested_path: None,
                canonical_path: None,
                optional: Some(false),
                status: "failed",
                span: Some(span_evidence(edge.span)),
                failure_code: edge.failure.map(|code| code.as_str().into()),
                truncated: edge.truncated,
            });
        }
        for target in &edge.targets {
            let (target_source_id, status) = squid_target_status(target.status);
            builder.dependency(SourceDependencyAdapter {
                source_id: edge.source.get(),
                target_source_id,
                requested_path: path_string(&target.requested_path),
                canonical_path: target.canonical_path.as_deref().and_then(path_string),
                optional: Some(false),
                status,
                span: Some(span_evidence(edge.span)),
                failure_code: edge.failure.map(|code| code.as_str().into()),
                truncated: edge.truncated,
            });
        }
    }
    builder.finish()
}

#[cfg(unix)]
fn squid_target_status(status: crate::squid::IncludeTargetStatus) -> (Option<u32>, &'static str) {
    match status {
        crate::squid::IncludeTargetStatus::Expanded(source) => (Some(source.get()), "expanded"),
        crate::squid::IncludeTargetStatus::Cycle(source) => (Some(source.get()), "cycle"),
        crate::squid::IncludeTargetStatus::SourceIo => (None, "source_io"),
        crate::squid::IncludeTargetStatus::SourceChanged => (None, "source_changed"),
        crate::squid::IncludeTargetStatus::SourceSizeLimit => (None, "source_size_limit"),
        crate::squid::IncludeTargetStatus::SourceFileLimit => (None, "source_file_limit"),
        crate::squid::IncludeTargetStatus::AggregateSourceLimit => (None, "aggregate_source_limit"),
        crate::squid::IncludeTargetStatus::ExpansionLimit => (None, "expansion_limit"),
        crate::squid::IncludeTargetStatus::UnsupportedPipe => (None, "unsupported_pipe"),
    }
}

pub(super) fn haproxy_graph<P: AsRef<Path>>(
    metadata: &SourceImportMetadata,
    roots: &[P],
) -> SourceGraphEvidence {
    let sources = metadata
        .original_sources
        .iter()
        .map(|source| source_reference(source, None))
        .collect::<Vec<_>>();
    let root_evidence = roots
        .iter()
        .enumerate()
        .map(|(ordinal, root)| {
            let path = root.as_ref();
            let source_ids = sources
                .iter()
                .filter(|source| {
                    source
                        .path
                        .as_deref()
                        .is_some_and(|loaded| path.to_str() == Some(loaded))
                })
                .map(|source| source.id)
                .collect::<Vec<u32>>();
            SourceRootEvidence {
                ordinal,
                path: path_string(path),
                outcome: Some(if source_ids.is_empty() {
                    "unknown".into()
                } else {
                    "loaded".into()
                }),
                source_ids,
            }
        })
        .collect();
    let snapshot_stable = if metadata.original_sources.is_empty() {
        None
    } else {
        Some(true)
    };
    SourceGraphEvidenceBuilder {
        roots: root_evidence,
        sources,
        dependencies: Vec::new(),
        dependencies_complete: false,
        snapshot_stable,
    }
    .finish()
}
