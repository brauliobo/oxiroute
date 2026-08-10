use crate::{
    ByteRange, Diagnostic, DiagnosticStage, E_SOURCE_LIMIT, MAX_DIRECTIVES_PER_SOURCE,
    MAX_SOURCE_BYTES, MAX_STRUCTURAL_DEPTH, MAX_TOKENS_PER_SOURCE, Report, Severity, SourceFile,
    SourceId, Span,
};

use super::{
    E_VCL_SYNTAX,
    lexer::{Token, TokenKind, lex},
};

include!("parser/ast.rs");
include!("parser/engine.rs");
