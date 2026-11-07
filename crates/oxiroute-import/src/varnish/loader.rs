use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::{
    ffi::OsString,
    fs, io,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::Component,
};

use crate::{
    Diagnostic, DiagnosticCode, DiagnosticStage, E_SOURCE_CHANGED, E_SOURCE_IO, E_SOURCE_LIMIT,
    MAX_AGGREGATE_SOURCE_BYTES, MAX_DIRECTIVES_PER_SOURCE, MAX_EXPANDED_DIRECTIVES,
    MAX_GLOB_MATCHES, MAX_INCLUDE_DEPTH, MAX_SOURCE_BYTES, MAX_SOURCE_FILES, MAX_STRUCTURAL_DEPTH,
    MAX_TOKENS_PER_SOURCE, Report, Severity, SourceFile, SourceId, Span,
    source::{SourceBudget, SourceCatalog, SourceCatalogFailure, SourceIdentity, SourceNaming},
};

use super::{
    Declaration, Document, E_VCL_INCLUDE_CYCLE, E_VCL_INCLUDE_NOT_FOUND, IncludeDeclaration,
    ParserLimits, Provenance, parse_with_limits,
};

mod filesystem;
mod glob;
mod memory;
mod model;

pub use filesystem::*;
use glob::{glob_matches, has_glob};
pub(super) use memory::*;
pub use model::*;

#[cfg(all(test, unix))]
mod tests;
