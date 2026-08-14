//! Bounded configuration source adapters and resolution into [`oxiroute_config::ValidatedConfig`].
//!
//! KDL follows the reversible mapping documented in `docs/CONFIG_FORMATS.md`: the document is an
//! implicit root object, scalar nodes have exactly one positional argument, `(object)` and
//! `(array)` nodes are explicit containers, and array children are named `-`. The decoder accepts
//! KDL 2 only. UCI uses named `config json` records rooted at `root`; each non-root record has a
//! `parent` plus either `key` or `index`, and every record has a `kind` and optional scalar `value`.
//! Source resolution additionally recognizes strict native nginx, `HAProxy`, Squid, Apache, and Varnish
//! directives without changing these generic reversible mappings.

mod composition;
mod error;
mod hocon;
mod kdl;
mod limits;
mod lua;
mod native;
mod render;
mod resolver;
mod templates;
mod uci;

use std::path::Path;

pub use composition::compose_validated_fragments;
pub use error::{ConfigSourceError, LuaConfigError, NativeDiagnosticCount, NativeDiagnostics};
pub use limits::{
    MAX_DEPENDENCY_PATHS, MAX_EXPANSION_DEPTH, MAX_NODES, MAX_OUTPUT_BYTES, MAX_SOURCE_BYTES,
    MAX_STRING_BYTES, MAX_STRUCTURAL_DEPTH, MAX_SUBSTITUTIONS,
};
pub use lua::load_lua;
pub use resolver::{
    NativeReferenceMetadata, ResolvedSource, resolve_source, resolve_source_with_format,
};
pub use templates::expand_templates;
pub use uci::{UciDocument, UciEntry, UciSection, parse_uci_document};

use oxiroute_config::ValidatedConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A supported configuration source syntax.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ConfigFormat {
    /// KDL 2.0, the default source and preview format.
    #[default]
    Kdl,
    /// Legacy restricted Lua, resolved through [`load_lua`].
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
/// This function performs no file or environment access. Lua is available only through
/// [`resolve_source`], which returns a typed configuration rather than a generic value.
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
/// HOCON output is sorted, pretty JSON, which is valid HOCON. Lua rendering remains unsupported;
/// resolved Lua sources receive a canonical KDL preview instead.
///
/// # Errors
///
/// Returns an error when the value exceeds a bound, cannot be represented by the selected format,
/// or uses the unsupported Lua adapter.
pub(crate) fn render_value(
    format: ConfigFormat,
    value: &Value,
) -> Result<String, ConfigSourceError> {
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

/// Deterministically renders a validated typed configuration.
///
/// # Errors
///
/// Returns an error when the selected format cannot represent the normalized value within the
/// configured bounds.
///
/// ```compile_fail
/// use oxiroute_config::ConfigDraft;
/// use oxiroute_config_source::{ConfigFormat, render_config};
///
/// let draft: ConfigDraft = todo!();
/// let _ = render_config(ConfigFormat::Kdl, &draft);
/// ```
pub fn render_config(
    format: ConfigFormat,
    config: &ValidatedConfig,
) -> Result<String, ConfigSourceError> {
    if format == ConfigFormat::Lua {
        return render::render_lua(config)
            .map_err(|error| ConfigSourceError::render_source("Lua", error));
    }
    let value = serde_json::to_value(config)
        .map_err(|error| ConfigSourceError::TypedConfig(error.to_string()))?;
    render_value(format, &value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn low_level_renderers_round_trip_without_becoming_public_configuration_apis() {
        for format in [ConfigFormat::Kdl, ConfigFormat::Uci, ConfigFormat::Hocon] {
            let value = json!({"z": [true, null], "a": {"two": 2, "one": 1}});
            let rendered = render_value(format, &value).unwrap();
            assert_eq!(decode_value(format, rendered.as_bytes()).unwrap(), value);
        }
    }

    #[test]
    fn kdl_renderer_requires_an_object_root() {
        assert!(matches!(
            render_value(ConfigFormat::Kdl, &json!([1, 2])),
            Err(ConfigSourceError::Render {
                format: "KDL 2",
                ..
            })
        ));
    }

    #[test]
    fn low_level_renderers_enforce_the_shared_output_bound() {
        let oversized = "x".repeat(MAX_STRING_BYTES);
        let value = Value::Object(
            ["a", "b", "c", "d", "e"]
                .into_iter()
                .map(|key| (key.to_owned(), Value::String(oversized.clone())))
                .collect(),
        );
        for format in [ConfigFormat::Kdl, ConfigFormat::Uci, ConfigFormat::Hocon] {
            assert!(matches!(
                render_value(format, &value),
                Err(ConfigSourceError::OutputTooLarge)
            ));
        }
    }
}
