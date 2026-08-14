use crate::{
    MAX_EXPANSION_DEPTH, MAX_NODES, MAX_OUTPUT_BYTES, MAX_SOURCE_BYTES, MAX_STRING_BYTES,
    MAX_STRUCTURAL_DEPTH, MAX_SUBSTITUTIONS,
};
use oxiroute_config::ConfigError;

/// Errors produced while loading or rendering restricted Lua configuration.
#[derive(Debug, thiserror::Error)]
pub enum LuaConfigError {
    #[error("Lua configuration failed: {0}")]
    Lua(#[source] mlua::Error),
    #[error("configuration exceeds the {MAX_SOURCE_BYTES}-byte source limit")]
    SourceTooLarge,
    #[error("{0}")]
    Config(#[from] ConfigError),
}

/// Count of native-import diagnostics carrying one stable machine-readable code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeDiagnosticCount {
    pub code: String,
    pub count: usize,
}

/// Bounded, content-free summary of diagnostics from a failed native import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeDiagnostics {
    pub counts: Vec<NativeDiagnosticCount>,
}

impl std::fmt::Display for NativeDiagnostics {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, entry) in self.counts.iter().enumerate() {
            if index != 0 {
                formatter.write_str(", ")?;
            }
            write!(formatter, "{}={}", entry.code, entry.count)?;
        }
        Ok(())
    }
}

/// Errors produced by source decoding, resolution, composition, and rendering.
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
    #[error("configuration exceeds the {MAX_SUBSTITUTIONS}-substitution limit")]
    SubstitutionLimit,
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
        #[source]
        source: Option<Box<dyn std::error::Error>>,
    },
    #[error("invalid template expansion: {0}")]
    Template(String),
    #[error("invalid typed configuration: {0}")]
    TypedConfig(String),
    #[error("configuration fragments cannot be composed: {0}")]
    Composition(String),
    #[error(transparent)]
    Lua(#[from] LuaConfigError),
    #[error("{importer} import did not produce a final candidate ({diagnostics})")]
    NativeImport {
        importer: &'static str,
        diagnostics: NativeDiagnostics,
    },
    #[error("configuration source does not contain an inline or native fragment")]
    NoFragments,
    #[error("native dependency count exceeds the supported limit")]
    DependencyLimit,
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
            source: None,
        }
    }

    pub(crate) fn render_source(
        format: &'static str,
        source: impl std::error::Error + 'static,
    ) -> Self {
        Self::Render {
            format,
            message: source.to_string(),
            source: Some(Box::new(source)),
        }
    }
}
