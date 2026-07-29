//! Bounded adapters between configuration source formats and [`serde_json::Value`].
//!
//! KDL follows the reversible mapping documented in `docs/CONFIG_FORMATS.md`: the document is an
//! implicit root object, scalar nodes have exactly one positional argument, `(object)` and
//! `(array)` nodes are explicit containers, and array children are named `-`. The decoder accepts
//! KDL 2 only. UCI uses named `config json` records rooted at `root`; each non-root record has a
//! `parent` plus either `key` or `index`, and every record has a `kind` and optional scalar `value`.

mod error;
mod hocon;
mod kdl;
mod limits;
mod templates;
mod uci;

use std::path::Path;

pub use error::ConfigSourceError;
pub use limits::{
    MAX_EXPANSION_DEPTH, MAX_NODES, MAX_OUTPUT_BYTES, MAX_SOURCE_BYTES, MAX_STRING_BYTES,
    MAX_STRUCTURAL_DEPTH,
};
pub use templates::expand_templates;
pub use uci::{UciDocument, UciEntry, UciSection, parse_uci_document, render_uci_document};

use serde_json::Value;

/// A supported configuration source syntax.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum ConfigFormat {
    /// KDL 2.0, the default source and preview format.
    #[default]
    Kdl,
    /// Legacy restricted Lua, implemented by `oxiroute-config` during this milestone.
    Lua,
    /// `OpenWrt` UCI using deterministic generic JSON records.
    Uci,
    /// HOCON parsed without includes or process environment access.
    Hocon,
}

impl ConfigFormat {
    /// Infers a format from a path extension. A path without an extension uses KDL.
    ///
    /// Extensions are matched case-insensitively. Supported values are `kdl`, `kdl2`, `lua`,
    /// `uci`, `hocon`, and `conf`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigSourceError::UnknownExtension`] for an unknown or non-UTF-8 extension.
    pub fn infer(path: impl AsRef<Path>) -> Result<Self, ConfigSourceError> {
        let Some(extension) = path.as_ref().extension() else {
            return Ok(Self::Kdl);
        };
        let extension = extension
            .to_str()
            .ok_or_else(|| ConfigSourceError::UnknownExtension("<non-UTF-8>".to_owned()))?;
        match extension.to_ascii_lowercase().as_str() {
            "kdl" | "kdl2" => Ok(Self::Kdl),
            "lua" => Ok(Self::Lua),
            "uci" => Ok(Self::Uci),
            "hocon" | "conf" => Ok(Self::Hocon),
            _ => Err(ConfigSourceError::UnknownExtension(extension.to_owned())),
        }
    }
}

/// Decodes already-supplied source bytes into the bounded format-neutral value tree.
///
/// This function performs no file or environment access. Lua intentionally remains unsupported in
/// this crate for milestone 1.
///
/// # Errors
///
/// Returns an error when the source exceeds a bound, is invalid UTF-8, uses an unsupported adapter,
/// or violates the selected format's strict mapping.
pub fn decode_value(format: ConfigFormat, source: &[u8]) -> Result<Value, ConfigSourceError> {
    limits::source_text(source).and_then(|source| match format {
        ConfigFormat::Kdl => kdl::decode(source),
        ConfigFormat::Uci => uci::decode(source),
        ConfigFormat::Hocon => hocon::decode(source),
        ConfigFormat::Lua => Err(ConfigSourceError::UnsupportedAdapter {
            format: "Lua",
            operation: "decode",
        }),
    })
}

/// Deterministically renders a bounded value tree in the selected format.
///
/// HOCON output is sorted, pretty JSON, which is valid HOCON. Lua intentionally remains
/// unsupported in this crate for milestone 1.
///
/// # Errors
///
/// Returns an error when the value exceeds a bound, cannot be represented by the selected format,
/// or uses the unsupported Lua adapter.
pub fn render_value(format: ConfigFormat, value: &Value) -> Result<String, ConfigSourceError> {
    limits::validate_value(value)?;
    let output = match format {
        ConfigFormat::Kdl => kdl::render(value)?,
        ConfigFormat::Uci => uci::render(value)?,
        ConfigFormat::Hocon => limits::render_sorted_json(value)?,
        ConfigFormat::Lua => {
            return Err(ConfigSourceError::UnsupportedAdapter {
                format: "Lua",
                operation: "render",
            });
        }
    };
    limits::check_output(&output)?;
    Ok(output)
}
