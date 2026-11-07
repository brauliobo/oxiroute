#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn source_metadata_evidence(metadata: &SourceImportMetadata) -> SourceMetadataEvidence {
    SourceMetadataEvidence {
        environment_fingerprint_sha256: metadata.environment_fingerprint_sha256.clone(),
        inactive_sources: metadata
            .inactive_sources
            .iter()
            .map(|source| InactiveSourceEvidence {
                condition: source.condition.clone(),
                origin: span_evidence(source.origin),
            })
            .collect(),
        original_source_ids: metadata
            .original_sources
            .iter()
            .map(|source| source.id().get())
            .collect(),
        source_maps: metadata
            .source_maps
            .iter()
            .map(source_map_evidence)
            .collect(),
    }
}

pub(super) fn graph_source_metadata<'a>(
    sources: impl Iterator<Item = &'a SourceFile>,
) -> SourceImportMetadata {
    SourceImportMetadata {
        original_sources: sources.cloned().collect(),
        ..SourceImportMetadata::default()
    }
}

fn source_map_evidence(map: &crate::SourceSpanMap) -> SourceMapEvidence {
    SourceMapEvidence {
        source_id: map.source.get(),
        segments: map
            .segments
            .iter()
            .map(|segment| SourceMapSegmentEvidence {
                generated: byte_range_evidence(segment.generated),
                original: byte_range_evidence(segment.original),
            })
            .collect(),
    }
}

pub(super) fn source_reference(
    source: &SourceFile,
    canonical_path: Option<&Path>,
) -> SourceReference {
    SourceReference {
        id: source.id().get(),
        name: source.name().to_owned(),
        path: canonical_path
            .and_then(path_string)
            .or_else(|| source.path().and_then(path_string)),
        byte_length: source.len(),
        fingerprint_sha256: fingerprint_sha256(source.bytes()),
    }
}

pub(super) fn span_evidence(span: Span) -> SpanEvidence {
    SpanEvidence {
        source_id: span.source().get(),
        range: byte_range_evidence(span.range()),
    }
}

fn byte_range_evidence(range: crate::ByteRange) -> ByteRangeEvidence {
    ByteRangeEvidence {
        start: range.start(),
        end: range.end(),
    }
}

pub(super) fn path_string(path: &Path) -> Option<String> {
    path.to_str().map(str::to_owned)
}

pub(super) fn bytes_string(bytes: &[u8]) -> Option<String> {
    String::from_utf8(bytes.to_vec()).ok()
}

fn fingerprint_sha256(bytes: &[u8]) -> String {
    let mut fingerprint = String::with_capacity(64);
    for byte in sha256(bytes) {
        write!(fingerprint, "{byte:02x}").expect("writing to a string cannot fail");
    }
    fingerprint
}

pub(super) fn source_fingerprint(
    sources: &[SourceReference],
    source_id: Option<u32>,
) -> Option<String> {
    source_id.and_then(|source_id| {
        sources
            .iter()
            .find(|source| source.id == source_id)
            .map(|source| source.fingerprint_sha256.clone())
    })
}

pub(super) fn source_roots(
    root: Option<SourceId>,
    sources: &[SourceReference],
) -> Vec<SourceRootEvidence> {
    let Some(root) = root else {
        return vec![SourceRootEvidence {
            ordinal: 0,
            path: None,
            source_ids: Vec::new(),
            outcome: Some("not_loaded".into()),
        }];
    };
    let source = sources.iter().find(|source| source.id == root.get());
    vec![SourceRootEvidence {
        ordinal: 0,
        path: source.and_then(|source| source.path.clone()),
        source_ids: vec![root.get()],
        outcome: Some("loaded".into()),
    }]
}

pub(super) fn nginx_origin(origin: &crate::nginx::DirectiveOrigin) -> OriginEvidence {
    OriginEvidence {
        role: None,
        source_id: origin.span.source().get(),
        range: Some(byte_range_evidence(origin.span.range())),
        path: None,
        line: None,
        include_stack: origin
            .provenance
            .include_stack
            .iter()
            .map(|frame| span_evidence(frame.directive_span))
            .collect(),
    }
}

pub(super) fn apache_origin(origin: &crate::apache::ApacheProvenance) -> OriginEvidence {
    OriginEvidence {
        role: Some(snake_case(&format!("{:?}", origin.role))),
        source_id: origin.span.source().get(),
        range: Some(byte_range_evidence(origin.span.range())),
        path: path_string(&origin.path),
        line: Some(origin.line),
        include_stack: origin
            .include_stack
            .iter()
            .map(|frame| span_evidence(frame.directive_span))
            .collect(),
    }
}

pub(super) fn varnish_origin(origin: &crate::varnish::Provenance) -> OriginEvidence {
    OriginEvidence {
        role: None,
        source_id: origin.span.source().get(),
        range: Some(byte_range_evidence(origin.span.range())),
        path: None,
        line: None,
        include_stack: origin
            .include_stack
            .iter()
            .copied()
            .map(span_evidence)
            .collect(),
    }
}

pub(super) fn haproxy_origin(origin: &crate::ProvenanceSpan) -> OriginEvidence {
    OriginEvidence {
        role: Some(snake_case(&format!("{:?}", origin.role))),
        source_id: origin.span.source().get(),
        range: Some(byte_range_evidence(origin.span.range())),
        path: None,
        line: None,
        include_stack: Vec::new(),
    }
}

#[cfg(unix)]
pub(super) fn squid_origin(origin: &crate::squid::DirectiveOrigin) -> OriginEvidence {
    OriginEvidence {
        role: None,
        source_id: origin.directive_span.source().get(),
        range: Some(byte_range_evidence(origin.directive_span.range())),
        path: None,
        line: None,
        include_stack: origin
            .provenance
            .include_stack
            .iter()
            .map(|frame| span_evidence(frame.directive_span))
            .collect(),
    }
}
