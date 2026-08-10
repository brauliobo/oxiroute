use std::collections::{BTreeMap, BTreeSet};

#[cfg(unix)]
use std::path::Path;

use crate::{
    CanonicalCandidate, Diagnostic, DiagnosticStage, E_DUPLICATE_IDENTITY, E_SOURCE_LIMIT, Report,
    Severity, SourceFile, Span,
};

use super::{
    AclDeclaration, Assignment, AssignmentOperator, BackendDeclaration, BackendDeclarationKind,
    BinaryOperator, Declaration, DirectorDeclaration, E_VCL_UNRESOLVED_REFERENCE,
    E_VCL_UNSUPPORTED, E_VCL_VERSION, Expression, ExpressionKind, IfStatement, ImportDeclaration,
    InvocationFacts, Literal, NewObjectStatement, ProbeDeclaration, SourceGraph, Statement,
    StatementKind, SubroutineDeclaration, UnaryOperator, VarnishdInvocation, VclVersion,
    loader::{LoadedDeclaration, load_memory},
};

const MAX_CALL_EDGES: usize = 16_384;
const MAX_CALL_DEPTH: usize = 64;
const MAX_CALL_WALK: usize = 100_000;
const MAX_CALL_CYCLES: usize = 1_024;

include!("semantic/model.rs");
include!("semantic/analyzer.rs");
