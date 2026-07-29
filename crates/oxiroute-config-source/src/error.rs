use crate::{
    MAX_EXPANSION_DEPTH, MAX_NODES, MAX_OUTPUT_BYTES, MAX_SOURCE_BYTES, MAX_STRING_BYTES,
    MAX_STRUCTURAL_DEPTH,
};

/// Errors produced by source decoding, rendering, and template expansion.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigSourceError {
    #[error("unknown configuration extension `{0}`")]
    UnknownExtension(String),
    #[error("{format} {operation} adapter is unsupported in oxiroute-config-source")]
    UnsupportedAdapter {
        format: &'static str,
        operation: &'static str,
    },
    #[error("source exceeds the {MAX_SOURCE_BYTES}-byte limit")]
    SourceTooLarge,
    #[error("rendered output exceeds the {MAX_OUTPUT_BYTES}-byte limit")]
    OutputTooLarge,
    #[error("configuration exceeds the {MAX_STRUCTURAL_DEPTH}-level structural depth limit")]
    StructuralDepth,
    #[error("template expansion exceeds the {MAX_EXPANSION_DEPTH}-level inheritance limit")]
    ExpansionDepth,
    #[error("configuration exceeds the {MAX_NODES}-node limit")]
    NodeLimit,
    #[error("configuration string exceeds the {MAX_STRING_BYTES}-byte limit")]
    StringLimit,
    #[error("configuration source is not valid UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("invalid {format}: {message}")]
    Parse {
        format: &'static str,
        message: String,
    },
    #[error("cannot render {format}: {message}")]
    Render {
        format: &'static str,
        message: String,
    },
    #[error("invalid template expansion: {0}")]
    Template(String),
}

impl ConfigSourceError {
    pub(crate) fn parse(format: &'static str, message: impl Into<String>) -> Self {
        Self::Parse {
            format,
            message: message.into(),
        }
    }

    pub(crate) fn render(format: &'static str, message: impl Into<String>) -> Self {
        Self::Render {
            format,
            message: message.into(),
        }
    }
}
