//! Strict, ordered Squid source loading, parsing, and import accounting.

mod bytes;
mod capabilities;
mod importer;
mod lexer;
mod loader;
mod parser;
mod semantic;
mod source;

use crate::DiagnosticCode;

pub use capabilities::{
    squid_capability_report, squid_directive_capabilities, SquidCapabilityFamily,
    SquidCapabilityProfile, SquidCapabilityReference, SquidCapabilityReport, SquidCapabilityStatus,
    SquidDirectiveCapability, SQUID_CAPABILITY_PROFILE_ID, SQUID_CAPABILITY_PROFILE_VERSION,
    SQUID_CAPABILITY_REGISTRY_VERSION, SQUID_TARGET_VERSION,
};
pub use importer::{
    BlockedCapability, ImportReport, LoweringView, SelectedImportReport, SquidLoweringAdapter,
    import, import_selected,
};
pub use lexer::{Line, QuoteStyle, Word, lex};
pub use loader::{
    ExpandedDirective, IncludeEdge, IncludeFrame, IncludeTarget, IncludeTargetStatus, OccurrenceId,
    ParsedSource, Provenance, SourceGraph, SquidLoadLimits, load, load_with_limits,
};
pub use parser::{Directive, Document, parse};
pub use semantic::*;
pub use source::{
    RootArgument, RootSelection, RootSelectionSource, SelectedSourceGraph, discover_root,
    load_selected, load_selected_with_limits,
};

/// Syntax errors produced while reading Squid source.
pub const E_SYNTAX: DiagnosticCode = DiagnosticCode::new("E_SYNTAX");
/// A Squid directive name is not registered by the strict classifier.
pub const E_UNKNOWN_DIRECTIVE: DiagnosticCode = DiagnosticCode::new("E_UNKNOWN_DIRECTIVE");
/// A registered directive does not have the required Squid form.
pub const E_UNSUPPORTED_FORM: DiagnosticCode = DiagnosticCode::new("E_UNSUPPORTED_FORM");
/// A reachable occurrence escaped terminal semantic accounting.
pub const E_UNCONSUMED_DIRECTIVE: DiagnosticCode = DiagnosticCode::new("E_UNCONSUMED_DIRECTIVE");

/// Maximum words accepted on one logical Squid configuration line.
pub const MAX_WORDS_PER_LINE: usize = 256;
