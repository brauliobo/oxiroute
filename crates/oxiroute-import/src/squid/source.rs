use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
};

use crate::{Diagnostic, DiagnosticStage, Report, Severity};

use super::{E_UNSUPPORTED_FORM, SourceGraph, SquidLoadLimits, load_with_limits};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootSelectionSource {
    CompiledDefault,
    CommandLine { argument_index: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootArgument {
    pub argument_index: usize,
    pub path: PathBuf,
}

/// Squid processes CLI arguments left-to-right and the final `-f` root wins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootSelection {
    pub compiled_default: PathBuf,
    pub command_line_roots: Vec<RootArgument>,
    pub active_root: PathBuf,
    pub source: RootSelectionSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedSourceGraph {
    pub selection: RootSelection,
    pub graph: SourceGraph,
}

#[must_use]
pub fn discover_root(arguments: &[OsString], compiled_default: &Path) -> Report<RootSelection> {
    let mut roots = Vec::new();
    let mut diagnostics = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == OsStr::new("--") {
            break;
        }
        if argument == OsStr::new("-f") {
            let Some(path) = arguments.get(index + 1) else {
                diagnostics.push(Diagnostic::new(
                    E_UNSUPPORTED_FORM,
                    Severity::Error,
                    DiagnosticStage::Source,
                    "Squid -f requires a configuration path",
                ));
                break;
            };
            roots.push(RootArgument {
                argument_index: index,
                path: PathBuf::from(path),
            });
            index += 2;
            continue;
        }
        let bytes = argument.as_encoded_bytes();
        if let Some(path) = bytes.strip_prefix(b"-f").filter(|path| !path.is_empty()) {
            roots.push(RootArgument {
                argument_index: index,
                path: PathBuf::from(OsString::from_vec(path.to_vec())),
            });
        }
        index += 1;
    }
    let (active_root, source) = roots.last().map_or_else(
        || {
            (
                compiled_default.to_path_buf(),
                RootSelectionSource::CompiledDefault,
            )
        },
        |root| {
            (
                root.path.clone(),
                RootSelectionSource::CommandLine {
                    argument_index: root.argument_index,
                },
            )
        },
    );
    Report::new(
        RootSelection {
            compiled_default: compiled_default.to_path_buf(),
            command_line_roots: roots,
            active_root,
            source,
        },
        diagnostics,
    )
}

#[must_use]
pub fn load_selected(
    arguments: &[OsString],
    compiled_default: &Path,
) -> Report<SelectedSourceGraph> {
    load_selected_with_limits(arguments, compiled_default, SquidLoadLimits::default())
}

#[must_use]
pub fn load_selected_with_limits(
    arguments: &[OsString],
    compiled_default: &Path,
    limits: SquidLoadLimits,
) -> Report<SelectedSourceGraph> {
    let (selection, mut diagnostics) = discover_root(arguments, compiled_default).into_parts();
    let (graph, load_diagnostics) = load_with_limits(&selection.active_root, limits).into_parts();
    diagnostics.extend(load_diagnostics);
    Report::new(SelectedSourceGraph { selection, graph }, diagnostics)
}
