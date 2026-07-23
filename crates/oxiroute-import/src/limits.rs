use crate::diagnostic::DiagnosticCode;

/// Maximum bytes parsed from one native source file (1 MiB).
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;

/// Maximum lexical tokens retained from one native source file.
pub const MAX_TOKENS_PER_SOURCE: usize = 1_000_000;

/// Maximum AST directives retained from one native source file.
pub const MAX_DIRECTIVES_PER_SOURCE: usize = 250_000;

/// Maximum nested structural blocks retained in one parsed source.
pub const MAX_STRUCTURAL_DEPTH: usize = 256;

/// Maximum active include edges from the root source.
pub const MAX_INCLUDE_DEPTH: usize = 64;

/// Maximum source file occurrences retained in one import operation.
pub const MAX_SOURCE_FILES: usize = 4_096;

/// Maximum aggregate bytes retained across source file occurrences (64 MiB).
pub const MAX_AGGREGATE_SOURCE_BYTES: usize = 64 * 1024 * 1024;

/// Maximum paths retained for one glob include occurrence.
pub const MAX_GLOB_MATCHES: usize = 100_000;

/// Maximum directive occurrences visited across all include expansions.
pub const MAX_EXPANDED_DIRECTIVES: usize = 1_000_000;

/// A source, token, or directive bound was exceeded.
pub const E_SOURCE_LIMIT: DiagnosticCode = DiagnosticCode::new("E_SOURCE_LIMIT");

/// A native source could not be read.
pub const E_SOURCE_IO: DiagnosticCode = DiagnosticCode::new("E_SOURCE_IO");

/// A source or glob changed while its graph was being loaded.
pub const E_SOURCE_CHANGED: DiagnosticCode = DiagnosticCode::new("E_SOURCE_CHANGED");

/// An exact include path did not exist.
pub const E_INCLUDE_NOT_FOUND: DiagnosticCode = DiagnosticCode::new("E_INCLUDE_NOT_FOUND");

/// An include reached a canonical source already on the active expansion stack.
pub const E_INCLUDE_CYCLE: DiagnosticCode = DiagnosticCode::new("E_INCLUDE_CYCLE");
