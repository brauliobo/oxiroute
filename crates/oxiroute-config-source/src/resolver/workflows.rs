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
