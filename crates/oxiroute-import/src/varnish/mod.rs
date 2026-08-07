//! Strict, non-executing Varnish VCL parsing, semantic classification, and exact lowering.
//!
//! Native typed evidence is retained for every source; canonical lowering finalizes only for the
//! explicitly supported VCL and varnishd invocation subset.

mod invocation;
mod lexer;
mod loader;
mod lower;
mod parser;
mod semantic;

pub use invocation::{
    InvocationFacts, MAX_INVOCATION_ARGUMENTS, MAX_INVOCATION_BYTES, Setting, StartupFact,
    StorageFact, StorageKind, VarnishdInvocation,
};
pub use loader::{
    IncludeEdge, IncludeTarget, IncludeTargetStatus, LoadedDeclaration, ParsedSource, SourceGraph,
    VarnishLoadLimits, VclVersion,
};
#[cfg(unix)]
pub use loader::{load, load_with_limits};
pub use lower::{
    VARNISH_CAPABILITY_PROFILE_ID, VARNISH_CAPABILITY_PROFILE_VERSION, VarnishCanonicalCandidate,
};
pub use parser::{
    AclDeclaration, AclEntry as ParsedAclEntry, Assignment, AssignmentOperator, BackendDeclaration,
    BackendDeclarationKind, BinaryOperator, ConditionalBranch, Declaration, DirectorDeclaration,
    DirectorEntry, Document, Expression, ExpressionKind, IfStatement, ImportDeclaration,
    IncludeDeclaration, Literal, NewObjectStatement, ParserLimits, ProbeDeclaration, Statement,
    StatementKind, SubroutineDeclaration, UnaryOperator, Value,
};
#[cfg(unix)]
pub use semantic::import;
pub use semantic::{
    Acl, AclEntry, Backend, BackendKind, BackendProperty, BackendReference, BuiltinComposition,
    CacheFlag, CacheLifetime, CacheLifetimeField, CallEdge, CallGraph, CompressionOperation,
    Condition, ConditionOperator, DeclarationClassification, DeclarationDecision, Director,
    DirectorKind, DirectorMethod, DynamicBehavior, FeatureBehavior, FlowAction, HeaderMutation,
    HeaderOperation, HeaderScope, ImportReport, Invalidation, LoweringBlocker, LoweringStatus,
    ModernDirector, Probe, ProbeProperty, ProbeReference, Provenance, ResponseAction,
    StatementClassification, StatementDecision, Subroutine, SubroutineComposition, SubroutineKind,
    UnsupportedBehavior, VersionContext, VersionOrigin, VmodImport, VmodObject, analyze,
    analyze_graph, decision_signatures,
};

use crate::DiagnosticCode;

/// VCL syntax is malformed or structurally incomplete.
pub const E_VCL_SYNTAX: DiagnosticCode = DiagnosticCode::new("E_VCL_SYNTAX");

/// An include cannot be resolved from the explicitly supplied source set.
pub const E_VCL_INCLUDE_NOT_FOUND: DiagnosticCode = DiagnosticCode::new("E_VCL_INCLUDE_NOT_FOUND");

/// An include repeats a source on the active expansion stack.
pub const E_VCL_INCLUDE_CYCLE: DiagnosticCode = DiagnosticCode::new("E_VCL_INCLUDE_CYCLE");

/// A native VCL behavior is retained as evidence but has no typed supported interpretation.
pub const E_VCL_UNSUPPORTED: DiagnosticCode = DiagnosticCode::new("E_VCL_UNSUPPORTED");

/// A backend or director reference has no unique declaration.
pub const E_VCL_UNRESOLVED_REFERENCE: DiagnosticCode =
    DiagnosticCode::new("E_VCL_UNRESOLVED_REFERENCE");

/// A declaration has no supported VCL version context.
pub const E_VCL_VERSION: DiagnosticCode = DiagnosticCode::new("E_VCL_VERSION");

/// The VCL graph contains behavior outside the exact canonical lowering subset.
pub const E_VCL_LOWERING_BLOCKED: DiagnosticCode = DiagnosticCode::new("E_VCL_LOWERING_BLOCKED");

/// The VCL graph uses a custom or unsupported subroutine edge.
pub const E_VCL_UNSUPPORTED_SUBROUTINE: DiagnosticCode =
    DiagnosticCode::new("E_VCL_UNSUPPORTED_SUBROUTINE");

/// A VCL value or phase has no semantics-preserving canonical representation.
pub const E_VCL_SEMANTIC_MISMATCH: DiagnosticCode = DiagnosticCode::new("E_VCL_SEMANTIC_MISMATCH");

pub use parser::{parse, parse_with_limits};
