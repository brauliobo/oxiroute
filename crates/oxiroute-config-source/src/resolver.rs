use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

use oxiroute_config::{ConfigDraft, ValidatedConfig};
use oxiroute_import::ImportReportEnvelope;
use serde_json::Value;

#[cfg(unix)]
use std::{
    ffi::OsString,
    os::unix::ffi::{OsStrExt as _, OsStringExt as _},
    path::Component,
};

use crate::error::{NativeDiagnosticCount, NativeDiagnostics};
use crate::native::{NativeDirective, extract_directives};
use crate::{
    ConfigFormat, ConfigSourceError, MAX_DEPENDENCY_PATHS, SourceOverrides, expand_templates,
    hocon, kdl, limits, lua, oxi, render_config, sugar, toml, uci, yaml,
};

/// A fully resolved, normalized configuration source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSource {
    /// Authored source syntax.
    pub format: ConfigFormat,
    /// Final normalized canonical configuration.
    pub config: ValidatedConfig,
    /// Deterministic KDL rendering of `config`, never the authored source.
    pub canonical_kdl: String,
    /// Whether templates, sugar, includes, embedded logic, or native source references
    /// contributed to `config`.
    pub compositional: bool,
    /// Native filesystem inputs needed for diagnostics and change watching.
    pub dependencies: Vec<PathBuf>,
    /// Directory-jailed configuration files included by `.oxi` or `.lua` sources.
    pub included_files: Vec<PathBuf>,
    /// Successful native imports retained with their product evidence and canonical provenance.
    pub native_references: Vec<NativeReferenceMetadata>,
}

/// Evidence retained for one successful native reference in a composed source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeReferenceMetadata {
    /// Resolved paths supplied to the native importer, in authored order.
    pub roots: Vec<PathBuf>,
    /// The same structured evidence emitted by the standalone import command.
    pub evidence: ImportReportEnvelope,
}

include!("resolver/workflows.rs");
include!("resolver/dependencies.rs");

/// Infers the authored syntax from `path` and resolves it into canonical configuration.
///
/// The supplied bytes are used only for the authored document. Native directives may read their
/// explicitly referenced filesystem roots through the existing restricted importers.
///
/// # Errors
///
/// Returns an error for invalid syntax, templates, native schemas or imports, typed configuration,
/// composition, canonical rendering, or a breached resource bound.
pub fn resolve_source(path: &Path, bytes: &[u8]) -> Result<ResolvedSource, ConfigSourceError> {
    resolve_source_with_format(path, bytes, ConfigFormat::infer(path)?)
}

/// Resolves supplied source bytes using an explicit authored syntax.
///
/// # Errors
///
/// Returns the same errors as [`resolve_source`].
pub fn resolve_source_with_format(
    path: &Path,
    bytes: &[u8],
    format: ConfigFormat,
) -> Result<ResolvedSource, ConfigSourceError> {
    resolve_source_with_overrides(path, bytes, format, None)
}

/// Resolves supplied source bytes using an explicit authored syntax and optional in-memory file
/// overrides for directory-jailed includes.
///
/// # Errors
///
/// Returns the same errors as [`resolve_source`].
pub fn resolve_source_with_overrides(
    path: &Path,
    bytes: &[u8],
    format: ConfigFormat,
    overrides: Option<&SourceOverrides>,
) -> Result<ResolvedSource, ConfigSourceError> {
    let source = limits::source_text(bytes)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut source_dependencies = Vec::new();
    let mut format_compositional = false;
    let (mut value, mut directives) = match format {
        ConfigFormat::Kdl => kdl::decode_with_directives(source)?,
        ConfigFormat::Hocon => {
            let (decoded, has_substitutions) = hocon::decode_with_meta(source)?;
            format_compositional = has_substitutions;
            (decoded, Vec::new())
        }
        ConfigFormat::Uci => {
            let (decoded, dirs, used_main) = uci::decode_with_directives(source)?;
            format_compositional = used_main;
            (decoded, dirs)
        }
        ConfigFormat::Yaml => (yaml::decode(source)?, Vec::new()),
        ConfigFormat::Toml => (toml::decode(source)?, Vec::new()),
        ConfigFormat::Lua => {
            let (decoded, deps) = lua::decode_with_base_dir(source, parent, overrides)?;
            format_compositional = lua_source_is_scripted(source);
            source_dependencies = deps;
            (decoded, Vec::new())
        }
        ConfigFormat::Oxi => {
            let (decoded, deps, oxi_compositional) =
                oxi::decode_with_base_dir(source, parent, overrides)?;
            format_compositional = oxi_compositional;
            source_dependencies = deps;
            (decoded, Vec::new())
        }
    };
    let format_name = format.display_name();
    let mut extracted = extract_directives(&mut value, format_name)?;
    extracted.append(&mut directives);
    directives = extracted;

    let has_templates = value
        .as_object()
        .is_some_and(|root| root.contains_key("templates") || root.contains_key("vars"));
    let value = expand_templates(&value)?;
    let (value, has_sugar) = sugar::desugar(value)?;
    let native_version = source_version(&value)?.unwrap_or(1);
    let inline = inline_config(value, format)?;
    let mut imported_fragments = Vec::with_capacity(directives.len());

    let mut dependencies = Dependencies::default();
    let has_source_dependencies = !source_dependencies.is_empty();
    let included_files = source_dependencies.clone();
    for dep in source_dependencies {
        dependencies.push_with_parent(&dep)?;
    }
    let mut native_references = Vec::with_capacity(directives.len());
    for directive in &directives {
        let imported = import_native(directive, parent, &mut dependencies)?;
        imported_fragments.push(imported.config);
        native_references.push(imported.metadata);
    }
    if inline.is_none() && imported_fragments.is_empty() {
        return Err(ConfigSourceError::NoFragments);
    }

    let config = crate::compose_validated_fragments(inline, imported_fragments, native_version)?;
    finish(
        format,
        config,
        format_compositional
            || has_templates
            || has_sugar
            || !directives.is_empty()
            || has_source_dependencies,
        dependencies.paths,
        included_files,
        native_references,
    )
}

/// Detects programmatic Lua constructs (statements before `return`, `local`, `function`, loops,
/// or `oxi.`/`oxiroute.` DSL calls) so typed saves don't silently destroy authored Lua logic,
/// while plain flat `return { ... }` data tables remain typed-editable.
fn lua_source_is_scripted(source: &str) -> bool {
    let stripped = strip_lua_comments_and_strings(source);
    let trimmed = stripped.trim_start();
    if !trimmed.starts_with("return") {
        return true;
    }
    let after_return = &trimmed["return".len()..];
    if after_return
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return true;
    }
    let body = after_return.trim_start();
    !body.starts_with('{')
}

fn strip_lua_comments_and_strings(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            out.push(' ');
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 2;
                } else if bytes[i] == quote {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn inline_config(
    mut value: Value,
    format: ConfigFormat,
) -> Result<Option<ConfigDraft>, ConfigSourceError> {
    let Value::Object(root) = &mut value else {
        return Err(ConfigSourceError::TypedConfig(
            "configuration root must be an object".to_owned(),
        ));
    };
    if root.keys().all(|key| key == "version") {
        return Ok(None);
    }
    if !root.contains_key("version") {
        return Err(ConfigSourceError::TypedConfig(
            "missing required top-level setting `version` (add `version 1`)".to_owned(),
        ));
    }
    root.entry("listeners")
        .or_insert_with(|| Value::Array(Vec::new()));
    if format == ConfigFormat::Lua {
        return lua::deserialize_draft(value).map(Some);
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|error| ConfigSourceError::TypedConfig(error.to_string()))
}

fn source_version(value: &Value) -> Result<Option<u32>, ConfigSourceError> {
    let Some(version) = value.as_object().and_then(|root| root.get("version")) else {
        return Ok(None);
    };
    version
        .as_u64()
        .and_then(|version| u32::try_from(version).ok())
        .map(Some)
        .ok_or_else(|| {
            ConfigSourceError::TypedConfig("version must be an unsigned 32-bit integer".to_owned())
        })
}

fn finish(
    format: ConfigFormat,
    config: ValidatedConfig,
    compositional: bool,
    dependencies: Vec<PathBuf>,
    included_files: Vec<PathBuf>,
    native_references: Vec<NativeReferenceMetadata>,
) -> Result<ResolvedSource, ConfigSourceError> {
    let canonical_kdl = render_config(ConfigFormat::Kdl, &config)?;
    Ok(ResolvedSource {
        format,
        config,
        canonical_kdl,
        compositional,
        dependencies,
        included_files,
        native_references,
    })
}
