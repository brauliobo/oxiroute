use std::path::PathBuf;

use oxiroute_import::{
    Report, SourceFile, SourceId,
    haproxy::{Configuration, EffectiveConfiguration, LoadedSource, parse_sources, resolve_parsed},
};

pub(super) fn resolve_bytes(contents: &[u8]) -> (Configuration, Report<EffectiveConfiguration>) {
    let parsed = parse_sources(&[loaded(0, "haproxy.cfg", contents)]);
    let configuration = parsed.value().clone();
    (configuration, resolve_parsed(parsed))
}

pub(super) fn loaded(source_id: u32, name: &str, contents: &[u8]) -> LoadedSource {
    let path = PathBuf::from(name);
    LoadedSource {
        root_ordinal: 0,
        file_ordinal: usize::try_from(source_id).unwrap(),
        source: SourceFile::from_path(SourceId::new(source_id), path.clone(), contents),
        path,
    }
}

pub(super) fn occurrence_count(configuration: &Configuration) -> usize {
    configuration
        .files
        .iter()
        .map(|file| {
            file.document.preamble.len()
                + file
                    .document
                    .sections
                    .iter()
                    .map(|section| 1 + section.directives.len())
                    .sum::<usize>()
        })
        .sum()
}

pub(super) fn code_count(
    report: &Report<EffectiveConfiguration>,
    code: oxiroute_import::DiagnosticCode,
) -> usize {
    report
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code() == code)
        .count()
}
