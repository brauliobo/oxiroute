//! Byte-level nginx lexical and structural parsing.

mod lexer;
#[cfg(unix)]
mod loader;
#[cfg(unix)]
mod lower;
mod parser;
#[cfg(unix)]
mod root;
#[cfg(unix)]
mod rtmp_lower;
#[cfg(unix)]
mod rtmp_semantic;
#[cfg(unix)]
mod semantic;

use crate::DiagnosticCode;

pub use lexer::{Token, TokenKind, lex};
#[cfg(unix)]
pub use loader::{
    ExpandedDirective, ExpandedOccurrence, IncludeCandidate, IncludeCandidateStatus, IncludeEdge,
    IncludeFrame, NginxLoadLimits, OccurrenceId, ParsedSource, Provenance, SourceGraph, load,
    load_with_limits,
};
#[cfg(unix)]
pub use lower::{BlockedService, ImportReport, import_http_fragment};
pub use parser::{Directive, Document, Word, parse};
#[cfg(unix)]
pub use root::{
    NginxBearerTokenOverlay, NginxDefaultAccessLogOverlay, NginxDefaultErrorPageOverlay,
    NginxHostTimezoneOverlay, NginxImportOptions, NginxImportReport, NginxRecordingRootOverlay,
    NginxUpstreamTlsOverlay, RootOccurrenceDecision, RootOccurrenceDisposition, import_root,
    import_root_with_options,
};
#[cfg(unix)]
pub use rtmp_lower::{
    BlockedRtmpService, RtmpImportReport, import_rtmp, import_rtmp_with_timezone,
};
#[cfg(unix)]
pub use rtmp_semantic::{
    EffectiveRtmp, EffectiveRtmpApplication, EffectiveRtmpListen, EffectiveRtmpPolicy,
    EffectiveRtmpPushTarget, EffectiveRtmpRecorder, EffectiveRtmpServer, RtmpRecordMode,
    RtmpResolution, resolve_rtmp,
};
#[cfg(unix)]
pub use semantic::{
    BoundServerName, DefaultServerSelection, DirectiveOrigin, EffectiveBind, EffectiveHttp,
    EffectiveListen, EffectiveLocation, EffectiveProxyPass, EffectiveServer, EffectiveServerName,
    EffectiveUpstream, EffectiveUpstreamServer, HttpDeclaration, HttpResolution, ListenEndpoint,
    LocationKind, NginxValue, OccurrenceDecision, OccurrenceDisposition, ProxyPassScheme,
    ServerDeclaration, ServerNameKind, StaticEndpoint, UpstreamReference, resolve_http_fragment,
};

/// Syntax errors produced while reading nginx source.
pub const E_SYNTAX: DiagnosticCode = DiagnosticCode::new("E_SYNTAX");
