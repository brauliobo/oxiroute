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

struct ImportedNative {
    config: Config,
    metadata: NativeReferenceMetadata,
}

#[cfg(unix)]
fn import_native(
    directive: &NativeDirective,
    parent: &Path,
    dependencies: &mut Dependencies,
) -> Result<ImportedNative, ConfigSourceError> {
    match directive {
        NativeDirective::Nginx(source) => import_nginx(source, parent, dependencies),
        NativeDirective::Haproxy(source) => import_haproxy(source, parent, dependencies),
        NativeDirective::Squid(source) => import_squid(source, parent, dependencies),
        NativeDirective::Apache(source) => import_apache(source, parent, dependencies),
        NativeDirective::Varnish(source) => import_varnish(source, parent, dependencies),
    }
}

#[cfg(unix)]
fn import_squid(
    source: &crate::native::SquidSource,
    parent: &Path,
    dependencies: &mut Dependencies,
) -> Result<ImportedNative, ConfigSourceError> {
    let path = resolve_path(parent, &source.path);
    let report = oxiroute_import::squid::import(&path);
    if !source.externalize_cache && !report.effective.refresh_policy.patterns.is_empty() {
        return Err(failed_native_import(
            "squid",
            report
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code() == oxiroute_import::E_UNSUPPORTED_FEATURE)
                .map(|diagnostic| diagnostic.code().as_str()),
        ));
    }
    let config = report.candidate.config().cloned().ok_or_else(|| {
        failed_native_import(
            "squid",
            report
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code().as_str()),
        )
    })?;
    push_squid_dependencies(&report.source_graph, dependencies)?;
    Ok(ImportedNative {
        config,
        metadata: NativeReferenceMetadata {
            roots: vec![path],
            evidence: oxiroute_import::ImportReportEnvelope::from_squid(&report),
        },
    })
}

#[cfg(unix)]
fn import_apache(
    source: &crate::native::ApacheSource,
    parent: &Path,
    dependencies: &mut Dependencies,
) -> Result<ImportedNative, ConfigSourceError> {
    let path = resolve_path(parent, &source.path);
    let report = oxiroute_import::apache::import_root(&path);
    let config = report.candidate.config().cloned().ok_or_else(|| {
        failed_native_import(
            "Apache",
            report
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code().as_str()),
        )
    })?;
    push_apache_dependencies(&report.source_graph, dependencies)?;
    Ok(ImportedNative {
        config,
        metadata: NativeReferenceMetadata {
            roots: vec![path],
            evidence: oxiroute_import::ImportReportEnvelope::from_apache(&report),
        },
    })
}

#[cfg(unix)]
fn import_varnish(
    source: &crate::native::VarnishSource,
    parent: &Path,
    dependencies: &mut Dependencies,
) -> Result<ImportedNative, ConfigSourceError> {
    let path = resolve_path(parent, &source.path);
    let invocation = oxiroute_import::varnish::VarnishdInvocation::new(source.arguments.clone());
    let report = oxiroute_import::varnish::import(&path, &invocation);
    let config = report.candidate.config().cloned().ok_or_else(|| {
        let lowering_code = match report.lowering {
            oxiroute_import::varnish::LoweringStatus::Lowered => None,
            oxiroute_import::varnish::LoweringStatus::Blocked(
                oxiroute_import::varnish::LoweringBlocker::UnsupportedSubroutine,
            ) => Some(oxiroute_import::varnish::E_VCL_UNSUPPORTED_SUBROUTINE.as_str()),
            oxiroute_import::varnish::LoweringStatus::Blocked(
                oxiroute_import::varnish::LoweringBlocker::SemanticMismatch,
            ) => Some(oxiroute_import::varnish::E_VCL_SEMANTIC_MISMATCH.as_str()),
            oxiroute_import::varnish::LoweringStatus::Blocked(
                oxiroute_import::varnish::LoweringBlocker::Validation,
            ) => Some(oxiroute_import::E_INVALID_VALUE.as_str()),
            oxiroute_import::varnish::LoweringStatus::Blocked(_) => {
                Some(oxiroute_import::varnish::E_VCL_LOWERING_BLOCKED.as_str())
            }
        };
        failed_native_import(
            "varnish",
            report
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code().as_str())
                .chain(lowering_code),
        )
    })?;
    push_varnish_dependencies(&report.source_graph, dependencies)?;
    Ok(ImportedNative {
        config,
        metadata: NativeReferenceMetadata {
            roots: vec![path],
            evidence: oxiroute_import::ImportReportEnvelope::from_varnish(&report),
        },
    })
}

#[cfg(not(unix))]
fn import_native(
    _directive: &NativeDirective,
    _parent: &Path,
    _dependencies: &mut Dependencies,
) -> Result<ImportedNative, ConfigSourceError> {
    Err(ConfigSourceError::UnsupportedAdapter {
        format: "native configuration",
        operation: "import",
    })
}

#[cfg(unix)]
fn import_nginx(
    source: &crate::native::NginxSource,
    parent: &Path,
    dependencies: &mut Dependencies,
) -> Result<ImportedNative, ConfigSourceError> {
    use oxiroute_import::nginx::{
        NginxDefaultAccessLogOverlay, NginxDefaultErrorPageOverlay, NginxHostTimezoneOverlay,
        NginxImportOptions, NginxRecordingRootOverlay, import_root_with_options,
    };

    let path = resolve_path(parent, &source.path);
    let root_prefix = resolve_path(parent, &source.root_prefix);
    let options = NginxImportOptions {
        host_timezones: source
            .host_timezone
            .as_ref()
            .map(|timezone| NginxHostTimezoneOverlay {
                timezone: timezone.clone(),
            })
            .into_iter()
            .collect(),
        default_access_log: source.default_access_log_file.as_ref().map(|path| {
            NginxDefaultAccessLogOverlay {
                path: resolve_path(parent, path),
            }
        }),
        recording_root: source
            .recording_root
            .as_ref()
            .map(|path| NginxRecordingRootOverlay {
                path: resolve_path(parent, path),
            }),
        default_error_page: source.default_error_server.as_ref().map(|server| {
            NginxDefaultErrorPageOverlay {
                server: server.clone(),
            }
        }),
        x_accel_controls_absent: source.x_accel_controls_absent,
        ..NginxImportOptions::default()
    };
    let report = import_root_with_options(&path, &root_prefix, &options);
    let config = report.candidate.config().cloned().ok_or_else(|| {
        failed_native_import(
            "nginx",
            report
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code().as_str()),
        )
    })?;
    push_nginx_dependencies(&report.source_graph, dependencies)?;
    Ok(ImportedNative {
        config,
        metadata: NativeReferenceMetadata {
            roots: vec![path],
            evidence: oxiroute_import::ImportReportEnvelope::from_nginx(&report),
        },
    })
}

#[cfg(unix)]
fn import_haproxy(
    source: &crate::native::HaproxySource,
    parent: &Path,
    dependencies: &mut Dependencies,
) -> Result<ImportedNative, ConfigSourceError> {
    use oxiroute_import::haproxy::{
        PreprocessingEnvironment, import_roots, import_roots_with_environment,
    };

    let paths = source
        .paths
        .iter()
        .map(|path| resolve_path(parent, path))
        .collect::<Vec<_>>();
    let report = source.node_ip.map_or_else(
        || import_roots(&paths),
        |node_ip| {
            import_roots_with_environment(
                &paths,
                PreprocessingEnvironment {
                    node_ip,
                    gpu1_defined: source.gpu1_defined,
                },
            )
        },
    );
    let candidate = report.value();
    let config = candidate.config().cloned().ok_or_else(|| {
        failed_native_import(
            "HAProxy",
            report
                .diagnostics()
                .iter()
                .map(|diagnostic| diagnostic.code().as_str()),
        )
    })?;
    for path in &paths {
        dependencies.push_with_parent(path)?;
    }
    for source in &candidate.source_metadata.original_sources {
        if let Some(path) = source.path() {
            dependencies.push_with_parent(path)?;
        }
    }
    Ok(ImportedNative {
        config,
        metadata: NativeReferenceMetadata {
            roots: paths.clone(),
            evidence: oxiroute_import::ImportReportEnvelope::from_haproxy(&report, &paths),
        },
    })
}

#[cfg(unix)]
fn push_nginx_dependencies(
    graph: &oxiroute_import::nginx::SourceGraph,
    dependencies: &mut Dependencies,
) -> Result<(), ConfigSourceError> {
    for source in &graph.sources {
        dependencies.push_with_parent(&source.canonical_path)?;
    }
    let include_base = graph
        .root
        .and_then(|root| graph.source(root))
        .and_then(|source| source.canonical_path.parent())
        .unwrap_or_else(|| Path::new("."));
    for edge in &graph.includes {
        dependencies.push_include_parent(include_base, &path_from_bytes(&edge.pattern))?;
    }
    Ok(())
}

#[cfg(unix)]
fn push_apache_dependencies(
    graph: &oxiroute_import::apache::SourceGraph,
    dependencies: &mut Dependencies,
) -> Result<(), ConfigSourceError> {
    for source in &graph.sources {
        dependencies.push_with_parent(&source.canonical_path)?;
    }
    let include_base = graph
        .root
        .and_then(|root| graph.source(root))
        .and_then(|source| source.canonical_path.parent())
        .unwrap_or_else(|| Path::new("."));
    for edge in &graph.includes {
        dependencies.push_include_parent(include_base, &path_from_bytes(&edge.pattern))?;
    }
    Ok(())
}

#[cfg(unix)]
fn push_varnish_dependencies(
    graph: &oxiroute_import::varnish::SourceGraph,
    dependencies: &mut Dependencies,
) -> Result<(), ConfigSourceError> {
    for source in &graph.sources {
        if let Some(path) = &source.canonical_path {
            dependencies.push_with_parent(path)?;
        }
    }
    let include_base = graph
        .root
        .and_then(|root| graph.source(root))
        .and_then(|source| source.canonical_path.as_deref())
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."));
    for edge in &graph.includes {
        dependencies.push_include_parent(include_base, &path_from_bytes(&edge.pattern))?;
    }
    Ok(())
}

#[cfg(unix)]
fn push_squid_dependencies(
    graph: &oxiroute_import::squid::SourceGraph,
    dependencies: &mut Dependencies,
) -> Result<(), ConfigSourceError> {
    for source in &graph.sources {
        dependencies.push_with_parent(&source.canonical_path)?;
    }
    for edge in &graph.includes {
        for target in &edge.targets {
            dependencies.push_with_parent(&target.requested_path)?;
        }
        if let Some(directive) = graph
            .expanded_directives
            .iter()
            .find(|directive| directive.occurrence == edge.occurrence)
        {
            let base = graph
                .source(edge.source)
                .and_then(|source| source.canonical_path.parent())
                .unwrap_or_else(|| Path::new("."));
            for argument in &directive.directive.arguments {
                dependencies.push_include_parent(base, &path_from_bytes(&argument.value))?;
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

#[cfg(unix)]
fn has_glob_meta(path: &Path) -> bool {
    let mut escaped = false;
    for byte in path.as_os_str().as_bytes() {
        if escaped {
            escaped = false;
        } else if *byte == b'\\' {
            escaped = true;
        } else if matches!(*byte, b'*' | b'?' | b'[') {
            return true;
        }
    }
    false
}

#[cfg(unix)]
fn include_watch_parent(base: &Path, requested: &Path) -> PathBuf {
    let path = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        base.join(requested)
    };
    let mut prefix = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => prefix.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => prefix.push(".."),
            Component::Normal(segment) if has_glob_meta(Path::new(segment)) => return prefix,
            Component::Normal(segment) => prefix.push(segment),
            Component::Prefix(_) => unreachable!("Unix paths have no prefix component"),
        }
    }
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or(prefix, Path::to_path_buf)
}

fn failed_native_import<'a>(
    importer: &'static str,
    codes: impl IntoIterator<Item = &'a str>,
) -> ConfigSourceError {
    let mut counts = BTreeMap::<String, usize>::new();
    for code in codes {
        *counts.entry(code.to_owned()).or_default() += 1;
    }
    if counts.is_empty() {
        counts.insert("E_NON_FINAL_CANDIDATE".to_owned(), 1);
    }
    ConfigSourceError::NativeImport {
        importer,
        diagnostics: NativeDiagnostics {
            counts: counts
                .into_iter()
                .map(|(code, count)| NativeDiagnosticCount { code, count })
                .collect(),
        },
    }
}

fn resolve_path(parent: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        parent.join(path)
    }
}

const fn format_name(format: ConfigFormat) -> &'static str {
    match format {
        ConfigFormat::Kdl => "KDL 2",
        ConfigFormat::Lua => "Lua",
        ConfigFormat::Uci => "UCI",
        ConfigFormat::Hocon => "HOCON",
    }
}

#[derive(Default)]
struct Dependencies {
    paths: Vec<PathBuf>,
    seen: HashSet<PathBuf>,
}

impl Dependencies {
    fn push(&mut self, path: PathBuf) -> Result<(), ConfigSourceError> {
        if self.seen.insert(path.clone()) {
            if self.paths.len() == MAX_DEPENDENCY_PATHS {
                return Err(ConfigSourceError::DependencyLimit);
            }
            self.paths.push(path);
        }
        Ok(())
    }

    fn push_with_parent(&mut self, path: &Path) -> Result<(), ConfigSourceError> {
        self.push(path.to_path_buf())?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            self.push(parent.to_path_buf())?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn push_include_parent(
        &mut self,
        base: &Path,
        requested: &Path,
    ) -> Result<(), ConfigSourceError> {
        self.push(include_watch_parent(base, requested))
    }
}
