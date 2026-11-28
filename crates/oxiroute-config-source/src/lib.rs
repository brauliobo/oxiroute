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
mod jail;
mod kdl;
mod limits;
mod lua;
mod native;
mod oxi;
mod render;
mod resolver;
mod sugar;
mod templates;
mod toml;
mod uci;
mod yaml;

use std::path::Path;

pub use composition::compose_validated_fragments;
pub use error::{ConfigSourceError, LuaConfigError, NativeDiagnosticCount, NativeDiagnostics};
pub use jail::{SourceOverrides, resolve_jailed_target, validate_relative_path};
pub use limits::{
    MAX_DEPENDENCY_PATHS, MAX_EXPANSION_DEPTH, MAX_NODES, MAX_OUTPUT_BYTES, MAX_SOURCE_BYTES,
    MAX_STRING_BYTES, MAX_STRUCTURAL_DEPTH, MAX_SUBSTITUTIONS,
};
pub use lua::load_lua;
pub use resolver::{
    NativeReferenceMetadata, ResolvedSource, resolve_source, resolve_source_with_format,
    resolve_source_with_overrides,
};
pub use templates::expand_templates;
pub use uci::{UciDocument, UciEntry, UciSection, parse_uci_document};

use oxiroute_config::ValidatedConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Canonical metadata for one supported configuration format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigFormatSpec {
    /// Strongly typed format identifier.
    pub format: ConfigFormat,
    /// Lowercase wire and CLI identifier (e.g. `"kdl"`, `"oxi"`).
    pub wire_name: &'static str,
    /// Human-readable display label (e.g. `"KDL 2"`, `"OXI"`).
    pub display_name: &'static str,
    /// Default file extension emitted by `init` and conversion helpers.
    pub default_extension: &'static str,
    /// All lowercase file extensions recognized for this format.
    pub extensions: &'static [&'static str],
    /// Whether this format can serialize explicit JSON `null` values.
    pub supports_null: bool,
}

/// Authoritative registry of all supported configuration formats, ordered by discovery preference.
pub const ALL_FORMATS: &[ConfigFormatSpec] = &[
    ConfigFormatSpec {
        format: ConfigFormat::Oxi,
        wire_name: "oxi",
        display_name: "OXI",
        default_extension: "oxi",
        extensions: &["oxi"],
        supports_null: true,
    },
    ConfigFormatSpec {
        format: ConfigFormat::Kdl,
        wire_name: "kdl",
        display_name: "KDL 2",
        default_extension: "kdl",
        extensions: &["kdl", "kdl2"],
        supports_null: true,
    },
    ConfigFormatSpec {
        format: ConfigFormat::Yaml,
        wire_name: "yaml",
        display_name: "YAML",
        default_extension: "yaml",
        extensions: &["yaml", "yml"],
        supports_null: true,
    },
    ConfigFormatSpec {
        format: ConfigFormat::Toml,
        wire_name: "toml",
        display_name: "TOML",
        default_extension: "toml",
        extensions: &["toml"],
        supports_null: false,
    },
    ConfigFormatSpec {
        format: ConfigFormat::Lua,
        wire_name: "lua",
        display_name: "Lua",
        default_extension: "lua",
        extensions: &["lua"],
        supports_null: true,
    },
    ConfigFormatSpec {
        format: ConfigFormat::Hocon,
        wire_name: "hocon",
        display_name: "HOCON",
        default_extension: "conf",
        extensions: &["conf", "hocon"],
        supports_null: true,
    },
    ConfigFormatSpec {
        format: ConfigFormat::Uci,
        wire_name: "uci",
        display_name: "UCI",
        default_extension: "uci",
        extensions: &["uci"],
        supports_null: true,
    },
];

/// A supported configuration source syntax.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ConfigFormat {
    /// KDL 2.0, the default source and preview format.
    #[default]
    Kdl,
    /// Sandboxed Lua configuration scripts.
    Lua,
    /// `OpenWrt` UCI using deterministic generic JSON records.
    Uci,
    /// HOCON parsed without includes or process environment access.
    Hocon,
    /// Strict YAML 1.2 parsed without aliases or multi-document streams.
    Yaml,
    /// TOML configuration format.
    Toml,
    /// `OxiRoute` stanza format (`.oxi`) with inline lists, `edge`/`site` sugar, and embedded Lua.
    Oxi,
}

impl ConfigFormat {
    /// Returns the authoritative metadata specification for this format.
    #[must_use]
    pub const fn spec(self) -> &'static ConfigFormatSpec {
        let mut index = 0;
        while index < ALL_FORMATS.len() {
            if matches!(
                (ALL_FORMATS[index].format, self),
                (Self::Kdl, Self::Kdl)
                    | (Self::Lua, Self::Lua)
                    | (Self::Uci, Self::Uci)
                    | (Self::Hocon, Self::Hocon)
                    | (Self::Yaml, Self::Yaml)
                    | (Self::Toml, Self::Toml)
                    | (Self::Oxi, Self::Oxi)
            ) {
                return &ALL_FORMATS[index];
            }
            index += 1;
        }
        &ALL_FORMATS[1]
    }

    /// Returns all supported formats in discovery preference order.
    #[must_use]
    pub const fn all() -> &'static [ConfigFormatSpec] {
        ALL_FORMATS
    }

    /// Returns the lowercase wire/CLI identifier for this format.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        self.spec().wire_name
    }

    /// Returns the human-readable display name for this format.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        self.spec().display_name
    }

    /// Returns the default file extension for this format.
    #[must_use]
    pub const fn default_extension(self) -> &'static str {
        self.spec().default_extension
    }

    /// Returns all recognized lowercase file extensions for this format.
    #[must_use]
    pub const fn extensions(self) -> &'static [&'static str] {
        self.spec().extensions
    }

    /// Returns whether this format can serialize explicit `null` values.
    #[must_use]
    pub const fn supports_null(self) -> bool {
        self.spec().supports_null
    }

    /// Parses a lowercase wire name (e.g. `"kdl"`, `"oxi"`, `"yaml"`).
    #[must_use]
    pub fn from_wire_name(name: &str) -> Option<Self> {
        ALL_FORMATS
            .iter()
            .find(|spec| spec.wire_name.eq_ignore_ascii_case(name))
            .map(|spec| spec.format)
    }

    /// Infers a format from a lowercase file extension without leading dot.
    #[must_use]
    pub fn from_extension(extension: &str) -> Option<Self> {
        let lower = extension.to_ascii_lowercase();
        ALL_FORMATS
            .iter()
            .find(|spec| spec.extensions.contains(&lower.as_str()))
            .map(|spec| spec.format)
    }

    /// Infers a format from a path extension. A path without an extension uses KDL.
    ///
    /// Extensions are matched case-insensitively. Supported values are `kdl`, `kdl2`, `lua`,
    /// `uci`, `hocon`, `conf`, `yaml`, `yml`, `toml`, and `oxi`.
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
        Self::from_extension(extension)
            .ok_or_else(|| ConfigSourceError::UnknownExtension(extension.to_owned()))
    }
}

/// Decodes already-supplied source bytes into the bounded format-neutral value tree.
///
/// This function performs no file or environment access.
///
/// # Errors
///
/// Returns an error when the source exceeds a bound, is invalid UTF-8, or violates the selected
/// format's strict mapping.
pub fn decode_value(format: ConfigFormat, source: &[u8]) -> Result<Value, ConfigSourceError> {
    limits::source_text(source).and_then(|source| match format {
        ConfigFormat::Kdl => kdl::decode(source),
        ConfigFormat::Uci => uci::decode(source),
        ConfigFormat::Hocon => hocon::decode(source),
        ConfigFormat::Yaml => yaml::decode(source),
        ConfigFormat::Toml => toml::decode(source),
        ConfigFormat::Lua => lua::decode(source),
        ConfigFormat::Oxi => oxi::decode(source),
    })
}

/// Deterministically renders a bounded value tree in the selected format.
///
/// HOCON output is sorted, pretty JSON, which is valid HOCON.
///
/// # Errors
///
/// Returns an error when the value exceeds a bound or cannot be represented by the selected format.
pub(crate) fn render_value(
    format: ConfigFormat,
    value: &Value,
) -> Result<String, ConfigSourceError> {
    limits::validate_value(value)?;
    let output = match format {
        ConfigFormat::Kdl => kdl::render(value)?,
        ConfigFormat::Uci => uci::render(value)?,
        ConfigFormat::Hocon => limits::render_sorted_json(value)?,
        ConfigFormat::Yaml => yaml::render(value)?,
        ConfigFormat::Toml => toml::render(value)?,
        ConfigFormat::Lua => lua::render(value)?,
        ConfigFormat::Oxi => oxi::render(value)?,
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
    let mut value = serde_json::to_value(config)
        .map_err(|error| ConfigSourceError::TypedConfig(error.to_string()))?;
    if format == ConfigFormat::Toml {
        strip_null_object_fields(&mut value);
        let round_tripped = serde_json::from_value::<oxiroute_config::ConfigDraft>(value.clone())
            .ok()
            .and_then(|draft| draft.validate().ok());
        if round_tripped.as_ref() != Some(config) {
            return Err(ConfigSourceError::render(
                "TOML",
                "TOML cannot represent explicit null overrides for fields with non-null defaults",
            ));
        }
    }
    render_value(format, &value)
}

fn strip_null_object_fields(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, entry| !entry.is_null());
            for entry in map.values_mut() {
                strip_null_object_fields(entry);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_null_object_fields(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn low_level_renderers_round_trip_without_becoming_public_configuration_apis() {
        for format in [
            ConfigFormat::Kdl,
            ConfigFormat::Uci,
            ConfigFormat::Hocon,
            ConfigFormat::Yaml,
            ConfigFormat::Lua,
            ConfigFormat::Oxi,
        ] {
            let value = json!({"z": [true, null], "a": {"two": 2, "one": 1}});
            let rendered = render_value(format, &value).unwrap();
            assert_eq!(decode_value(format, rendered.as_bytes()).unwrap(), value);
        }

        let toml_value = json!({"z": [true, false], "a": {"two": 2, "one": 1}});
        let rendered = render_value(ConfigFormat::Toml, &toml_value).unwrap();
        assert_eq!(
            decode_value(ConfigFormat::Toml, rendered.as_bytes()).unwrap(),
            toml_value
        );
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
            (0..=MAX_OUTPUT_BYTES / MAX_STRING_BYTES)
                .map(|key| (format!("key-{key}"), Value::String(oversized.clone())))
                .collect(),
        );
        for format in [
            ConfigFormat::Kdl,
            ConfigFormat::Uci,
            ConfigFormat::Hocon,
            ConfigFormat::Yaml,
            ConfigFormat::Toml,
            ConfigFormat::Lua,
            ConfigFormat::Oxi,
        ] {
            assert!(matches!(
                render_value(format, &value),
                Err(ConfigSourceError::OutputTooLarge)
            ));
        }
    }
}
