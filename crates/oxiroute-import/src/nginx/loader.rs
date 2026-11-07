use std::{
    ffi::OsString,
    fs, io,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, PathBuf},
};

use crate::{
    Diagnostic, DiagnosticCode, DiagnosticStage, E_INCLUDE_CYCLE, E_INCLUDE_NOT_FOUND,
    E_SOURCE_CHANGED, E_SOURCE_IO, E_SOURCE_LIMIT, E_UNSUPPORTED_FEATURE,
    MAX_AGGREGATE_SOURCE_BYTES, MAX_DIRECTIVES_PER_SOURCE, MAX_EXPANDED_DIRECTIVES,
    MAX_GLOB_MATCHES, MAX_INCLUDE_DEPTH, MAX_SOURCE_BYTES, MAX_SOURCE_FILES, MAX_STRUCTURAL_DEPTH,
    MAX_TOKENS_PER_SOURCE, Report, Severity, SourceFile, SourceId, Span,
    source::{SourceBudget, SourceCatalog, SourceCatalogFailure, SourceIdentity, SourceNaming},
};

use super::{Directive, Document, E_SYNTAX, parser::parse_with_limits};

mod engine;
mod model;
mod resolve;

pub use engine::*;
pub use model::*;
use resolve::{Resolution, ResolveFailure, canonical_path_changed, resolve_pattern};

#[cfg(test)]
mod tests;
