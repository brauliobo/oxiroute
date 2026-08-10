use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

use oxiroute_config::{Config, compose_configs, load_lua};
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
    ConfigFormat, ConfigSourceError, MAX_DEPENDENCY_PATHS, expand_templates, hocon, kdl, limits,
    render_config, uci,
};

/// A fully resolved, normalized configuration source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSource {
    /// Authored source syntax.
    pub format: ConfigFormat,
    /// Final normalized canonical configuration.
    pub config: Config,
    /// Deterministic KDL rendering of `config`, never the authored source.
    pub canonical_kdl: String,
    /// Whether templates or native source references contributed to `config`.
    pub compositional: bool,
    /// Native filesystem inputs needed for diagnostics and change watching.
    pub dependencies: Vec<PathBuf>,
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
    let source = limits::source_text(bytes)?;
    if format == ConfigFormat::Lua {
        let config = load_lua(source).map_err(|error| ConfigSourceError::Lua(error.to_string()))?;
        return finish(format, config, false, Vec::new(), Vec::new());
    }

    let (mut value, mut directives) = match format {
        ConfigFormat::Kdl => kdl::decode_with_directives(source)?,
        ConfigFormat::Hocon => (hocon::decode(source)?, Vec::new()),
        ConfigFormat::Uci => uci::decode_with_directives(source)?,
        ConfigFormat::Lua => unreachable!("Lua is resolved before generic decoding"),
    };
    let format_name = format_name(format);
    let mut extracted = extract_directives(&mut value, format_name)?;
    extracted.append(&mut directives);
    directives = extracted;

    let has_templates = value
        .as_object()
        .is_some_and(|root| root.contains_key("templates"));
    let value = expand_templates(&value)?;
    let native_version = source_version(&value)?.unwrap_or(1);
    let inline = inline_config(value)?;
    let mut fragments = Vec::with_capacity(directives.len() + usize::from(inline.is_some()));
    if let Some(config) = inline {
        fragments.push(config);
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut dependencies = Dependencies::default();
    let mut native_references = Vec::with_capacity(directives.len());
    for directive in &directives {
        let imported = import_native(directive, parent, &mut dependencies)?;
        let mut config = imported.config;
        config.version = fragments
            .first()
            .map_or(native_version, |inline| inline.version);
        fragments.push(config);
        native_references.push(imported.metadata);
    }
    if fragments.is_empty() {
        return Err(ConfigSourceError::NoFragments);
    }

    let config = compose_configs(&fragments)
        .map_err(|error| ConfigSourceError::Composition(error.to_string()))?;
    finish(
        format,
        config,
        has_templates || !directives.is_empty(),
        dependencies.paths,
        native_references,
    )
}

fn inline_config(mut value: Value) -> Result<Option<Config>, ConfigSourceError> {
    let Value::Object(root) = &mut value else {
        return Err(ConfigSourceError::TypedConfig(
            "configuration root must be an object".to_owned(),
        ));
    };
    if root.keys().all(|key| key == "version") {
        return Ok(None);
    }
    root.entry("listeners")
        .or_insert_with(|| Value::Array(Vec::new()));
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
    config: Config,
    compositional: bool,
    dependencies: Vec<PathBuf>,
    native_references: Vec<NativeReferenceMetadata>,
) -> Result<ResolvedSource, ConfigSourceError> {
    let config = compose_configs(&[config])
        .map_err(|error| ConfigSourceError::Composition(error.to_string()))?;
    let canonical_kdl = render_config(ConfigFormat::Kdl, &config)?;
    Ok(ResolvedSource {
        format,
        config,
        canonical_kdl,
        compositional,
        dependencies,
        native_references,
    })
}
