use std::{fmt::Write as _, net::IpAddr};

use openssl::sha::sha256;

use crate::{
    ByteRange, Diagnostic, DiagnosticStage, InactiveSource, Report, Severity, SourceFile,
    SourceImportMetadata, SourceMapSegment, SourceSpanMap, Span,
};

use super::{E_CONDITIONAL_PREPROCESSING, E_ENVIRONMENT_EXPANSION, LoadedRoots, LoadedSource};

const NODE_IP_REFERENCE: &[u8] = b"${NODE_IP}";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreprocessingEnvironment {
    pub node_ip: IpAddr,
    pub gpu1_defined: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PreprocessedSources {
    pub sources: Vec<LoadedSource>,
    pub original_sources: Vec<LoadedSource>,
    pub source_maps: Vec<SourceSpanMap>,
    pub root_decisions: Vec<super::RootLoadDecision>,
    pub metadata: SourceImportMetadata,
}

#[derive(Clone, Copy)]
struct ConditionalFrame {
    parent_active: bool,
    condition: bool,
    in_else: bool,
}

struct MappedOutput {
    bytes: Vec<u8>,
    source_map: SourceSpanMap,
}

impl MappedOutput {
    fn new(source: &LoadedSource) -> Self {
        Self {
            bytes: Vec::with_capacity(source.source.len()),
            source_map: SourceSpanMap {
                source: source.source.id(),
                segments: Vec::new(),
            },
        }
    }

    fn push(&mut self, bytes: &[u8], original: ByteRange) {
        if bytes.is_empty() {
            return;
        }
        let start = self.bytes.len();
        self.bytes.extend_from_slice(bytes);
        self.source_map.segments.push(SourceMapSegment {
            generated: ByteRange::new(start, self.bytes.len()),
            original,
        });
    }
}

impl ConditionalFrame {
    const fn active(self) -> bool {
        self.parent_active
            && if self.in_else {
                !self.condition
            } else {
                self.condition
            }
    }
}

#[must_use]
pub fn preprocess_sources(
    loaded: &LoadedRoots,
    environment: PreprocessingEnvironment,
) -> Report<PreprocessedSources> {
    let mut sources = Vec::with_capacity(loaded.sources.len());
    let mut inactive_sources = Vec::new();
    let mut diagnostics = Vec::new();
    let mut source_maps = Vec::with_capacity(loaded.sources.len());

    for source in &loaded.sources {
        let output =
            preprocess_source(source, environment, &mut inactive_sources, &mut diagnostics);
        source_maps.push(output.source_map);
        sources.push(LoadedSource {
            root_ordinal: source.root_ordinal,
            file_ordinal: source.file_ordinal,
            path: source.path.clone(),
            source: SourceFile::from_path(source.source.id(), &source.path, output.bytes),
        });
    }

    Report::new(
        PreprocessedSources {
            sources,
            original_sources: loaded.sources.clone(),
            source_maps: source_maps.clone(),
            root_decisions: loaded.decisions.clone(),
            metadata: SourceImportMetadata {
                environment_fingerprint_sha256: Some(environment_fingerprint(environment)),
                inactive_sources,
                original_sources: loaded
                    .sources
                    .iter()
                    .map(|source| source.source.clone())
                    .collect(),
                source_maps,
            },
        },
        diagnostics,
    )
}

fn preprocess_source(
    source: &LoadedSource,
    environment: PreprocessingEnvironment,
    inactive_sources: &mut Vec<InactiveSource>,
    diagnostics: &mut Vec<Diagnostic>,
) -> MappedOutput {
    let input = source.source.bytes();
    let mut output = MappedOutput::new(source);
    let mut frames = Vec::<ConditionalFrame>::new();
    let mut offset = 0;

    for line in input.split_inclusive(|byte| *byte == b'\n') {
        let line_end = offset + line.len();
        let content = line
            .strip_suffix(b"\n")
            .unwrap_or(line)
            .strip_suffix(b"\r")
            .unwrap_or_else(|| line.strip_suffix(b"\n").unwrap_or(line));
        let trimmed = trim_ascii(content);
        let parent_active = frames.last().is_none_or(|frame| frame.active());

        match trimmed {
            b".if defined(GPU1)" => {
                frames.push(ConditionalFrame {
                    parent_active,
                    condition: environment.gpu1_defined,
                    in_else: false,
                });
                retain_line_ending(line, offset, &mut output);
            }
            b".else" => {
                let Some(frame) = frames.last_mut() else {
                    diagnostics.push(conditional_error(
                        source,
                        offset,
                        line_end,
                        "HAProxy `.else` has no matching `.if`",
                    ));
                    retain_line_ending(line, offset, &mut output);
                    offset = line_end;
                    continue;
                };
                if frame.in_else {
                    diagnostics.push(conditional_error(
                        source,
                        offset,
                        line_end,
                        "HAProxy conditional contains more than one `.else`",
                    ));
                }
                frame.in_else = true;
                retain_line_ending(line, offset, &mut output);
            }
            b".endif" => {
                if frames.pop().is_none() {
                    diagnostics.push(conditional_error(
                        source,
                        offset,
                        line_end,
                        "HAProxy `.endif` has no matching `.if`",
                    ));
                }
                retain_line_ending(line, offset, &mut output);
            }
            _ if trimmed.starts_with(b".if")
                || trimmed.starts_with(b".elif")
                || trimmed.starts_with(b".else")
                || trimmed.starts_with(b".endif") =>
            {
                diagnostics.push(conditional_error(
                    source,
                    offset,
                    line_end,
                    "only `.if defined(GPU1)`, `.else`, and `.endif` are accepted by the deterministic HAProxy preprocessor",
                ));
                retain_line_ending(line, offset, &mut output);
            }
            _ if parent_active => expand_node_ip(
                line,
                source,
                offset,
                environment.node_ip,
                &mut output,
                diagnostics,
            ),
            _ => {
                inactive_sources.push(InactiveSource {
                    condition: inactive_condition(&frames),
                    origin: Span::new(source.source.id(), ByteRange::new(offset, line_end)),
                });
                retain_line_ending(line, offset, &mut output);
            }
        }
        offset = line_end;
    }

    if !frames.is_empty() {
        diagnostics.push(conditional_error(
            source,
            input.len(),
            input.len(),
            "HAProxy conditional is missing `.endif`",
        ));
    }
    output
}

fn expand_node_ip(
    line: &[u8],
    source: &LoadedSource,
    line_offset: usize,
    node_ip: IpAddr,
    output: &mut MappedOutput,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let replacement = node_ip.to_string();
    let mut remaining = line;
    let mut consumed = 0;
    while let Some(index) = find_bytes(remaining, b"${") {
        output.push(
            &remaining[..index],
            ByteRange::new(line_offset + consumed, line_offset + consumed + index),
        );
        let reference = &remaining[index..];
        if reference.starts_with(NODE_IP_REFERENCE) {
            output.push(
                replacement.as_bytes(),
                ByteRange::new(
                    line_offset + consumed + index,
                    line_offset + consumed + index + NODE_IP_REFERENCE.len(),
                ),
            );
            remaining = &reference[NODE_IP_REFERENCE.len()..];
            consumed += index + NODE_IP_REFERENCE.len();
        } else {
            let end = reference
                .iter()
                .position(|byte| *byte == b'}')
                .map_or(reference.len(), |index| index + 1);
            diagnostics.push(
                Diagnostic::new(
                    E_ENVIRONMENT_EXPANSION,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    "only `${NODE_IP}` is accepted by the deterministic HAProxy preprocessor",
                )
                .with_primary_span(Span::new(
                    source.source.id(),
                    ByteRange::new(
                        line_offset + consumed + index,
                        line_offset + consumed + index + end,
                    ),
                )),
            );
            output.push(
                &reference[..end],
                ByteRange::new(
                    line_offset + consumed + index,
                    line_offset + consumed + index + end,
                ),
            );
            remaining = &reference[end..];
            consumed += index + end;
        }
    }
    output.push(
        remaining,
        ByteRange::new(
            line_offset + consumed,
            line_offset + consumed + remaining.len(),
        ),
    );
}

fn conditional_error(
    source: &LoadedSource,
    start: usize,
    end: usize,
    message: &'static str,
) -> Diagnostic {
    Diagnostic::new(
        E_CONDITIONAL_PREPROCESSING,
        Severity::Error,
        DiagnosticStage::Resolve,
        message,
    )
    .with_primary_span(Span::new(source.source.id(), ByteRange::new(start, end)))
}

fn environment_fingerprint(environment: PreprocessingEnvironment) -> String {
    let material = format!(
        "NODE_IP={}\nGPU1={}\n",
        environment.node_ip,
        u8::from(environment.gpu1_defined)
    );
    sha256(material.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut encoded, byte| {
            write!(encoded, "{byte:02x}").expect("writing to a string cannot fail");
            encoded
        })
}

fn inactive_condition(frames: &[ConditionalFrame]) -> String {
    frames
        .iter()
        .rev()
        .find(|frame| !frame.active())
        .map_or_else(
            || "inactive parent condition".to_owned(),
            |frame| {
                if frame.in_else {
                    "not defined(GPU1)".to_owned()
                } else {
                    "defined(GPU1)".to_owned()
                }
            },
        )
}

fn retain_line_ending(line: &[u8], line_offset: usize, output: &mut MappedOutput) {
    if line.ends_with(b"\r\n") {
        output.push(
            b"\r\n",
            ByteRange::new(line_offset + line.len() - 2, line_offset + line.len()),
        );
    } else if line.ends_with(b"\n") {
        output.push(
            b"\n",
            ByteRange::new(line_offset + line.len() - 1, line_offset + line.len()),
        );
    }
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
