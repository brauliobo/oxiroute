#[allow(clippy::wildcard_imports)]
use super::*;

/// Executes a non-serving local command and returns its stdout body.
///
/// # Errors
///
/// Returns a redacted validation or import failure. A report containing diagnostics is successful;
/// preview output requires a fully finalized canonical configuration.
#[allow(clippy::too_many_lines)]
pub fn execute_offline(command: &Command) -> Result<Option<String>, Box<dyn Error>> {
    match command {
        Command::Version => Ok(Some(format!("{BUILD_VERSION}\n"))),
        Command::Config {
            command: ConfigCommand::Check { config },
        } => {
            check_config(config)?;
            Ok(Some(
                "configuration is valid and runtime-preparable\n".into(),
            ))
        }
        Command::Config {
            command: ConfigCommand::Compose { format, configs },
        } => Ok(Some(compose_config_files(configs, (*format).into())?)),
        Command::Import {
            command:
                ImportCommand::Nginx {
                    config,
                    root_prefix,
                    host_timezone,
                    default_access_log_file,
                    recording_root,
                    default_error_server,
                    x_accel_controls_absent,
                    shadow_port_offset,
                    format,
                    output,
                },
        } => Ok(Some(import_nginx(
            config,
            root_prefix,
            host_timezone.as_deref(),
            default_access_log_file.as_deref(),
            recording_root.as_deref(),
            default_error_server.as_deref(),
            *x_accel_controls_absent,
            *shadow_port_offset,
            (*format).into(),
            *output,
        )?)),
        Command::Import {
            command:
                ImportCommand::Haproxy {
                    configs,
                    node_ip,
                    gpu1_defined,
                    shadow_port_offset,
                    format,
                    output,
                },
        } => Ok(Some(import_haproxy(
            configs,
            *node_ip,
            *gpu1_defined,
            *shadow_port_offset,
            (*format).into(),
            *output,
        )?)),
        Command::Import {
            command:
                ImportCommand::Squid {
                    config,
                    shadow_port_offset,
                    format,
                    output,
                },
        } => Ok(Some(import_squid(
            config,
            *shadow_port_offset,
            (*format).into(),
            *output,
        )?)),
        Command::Import {
            command:
                ImportCommand::Apache {
                    config,
                    shadow_port_offset,
                    format,
                    output,
                },
        } => Ok(Some(import_apache(
            config,
            *shadow_port_offset,
            (*format).into(),
            *output,
        )?)),
        Command::Import {
            command:
                ImportCommand::Varnish {
                    config,
                    arguments,
                    shadow_port_offset,
                    format,
                    output,
                },
        } => Ok(Some(import_varnish(
            config,
            arguments,
            *shadow_port_offset,
            (*format).into(),
            *output,
        )?)),
        _ => Ok(None),
    }
}

fn check_config(path: &Path) -> Result<(), Box<dyn Error>> {
    let coordinator = CanonicalConfigCoordinator::new(path)?;
    let document = match coordinator.load() {
        ConfigLoadOutcome::Loaded(document) => document,
        ConfigLoadOutcome::Rejected(rejection) => return Err(rejection.message().into()),
    };
    GenerationManager::new()
        .validate_candidate(*document)
        .map_err(|error| {
            format!("configuration cannot be prepared as a complete runtime generation: {error}")
        })?;
    Ok(())
}

fn compose_config_files(paths: &[PathBuf], format: ConfigFormat) -> Result<String, Box<dyn Error>> {
    let mut configs = Vec::with_capacity(paths.len());
    for path in paths {
        let coordinator = CanonicalConfigCoordinator::new(path)?;
        let document = match coordinator.load() {
            ConfigLoadOutcome::Loaded(document) => document,
            ConfigLoadOutcome::Rejected(rejection) => {
                return Err(format!("`{}`: {}", path.display(), rejection.message()).into());
            }
        };
        configs.push(document.validated_config.to_draft());
    }
    let composed = oxiroute_config::compose_validated_configs(configs)?;
    Ok(render_config(format, &composed)?)
}

#[expect(
    clippy::too_many_arguments,
    reason = "offline nginx import inputs map directly to explicit operator overlays"
)]
fn import_nginx(
    path: &Path,
    root_prefix: &Path,
    host_timezone: Option<&str>,
    default_access_log_file: Option<&Path>,
    recording_root: Option<&Path>,
    default_error_server: Option<&str>,
    x_accel_controls_absent: bool,
    shadow_port_offset: Option<u16>,
    format: ConfigFormat,
    output: ImportOutput,
) -> Result<String, Box<dyn Error>> {
    let options = oxiroute_import::nginx::NginxImportOptions {
        host_timezones: host_timezone
            .map(
                |timezone| oxiroute_import::nginx::NginxHostTimezoneOverlay {
                    timezone: timezone.into(),
                },
            )
            .into_iter()
            .collect(),
        default_access_log: default_access_log_file.map(|path| {
            oxiroute_import::nginx::NginxDefaultAccessLogOverlay {
                path: path.to_path_buf(),
            }
        }),
        recording_root: recording_root.map(|path| {
            oxiroute_import::nginx::NginxRecordingRootOverlay {
                path: path.to_path_buf(),
            }
        }),
        default_error_page: default_error_server.map(|server| {
            oxiroute_import::nginx::NginxDefaultErrorPageOverlay {
                server: server.to_owned(),
            }
        }),
        x_accel_controls_absent,
        ..oxiroute_import::nginx::NginxImportOptions::default()
    };
    let report = oxiroute_import::nginx::import_root_with_options(path, root_prefix, &options);
    match output {
        ImportOutput::Preview => preview_with_shadow_listener_offset(
            report.candidate.validated(),
            shadow_port_offset,
            format,
        ),
        ImportOutput::Report => {
            if shadow_port_offset.is_some() {
                return Err("--shadow-port-offset requires --output preview".into());
            }
            Ok(oxiroute_import::ImportReportEnvelope::from_nginx(&report).to_json_line()?)
        }
    }
}

pub(super) fn import_haproxy(
    paths: &[PathBuf],
    node_ip: Option<IpAddr>,
    gpu1_defined: bool,
    shadow_port_offset: Option<u16>,
    format: ConfigFormat,
    output: ImportOutput,
) -> Result<String, Box<dyn Error>> {
    let report = node_ip.map_or_else(
        || oxiroute_import::haproxy::import_roots(paths),
        |node_ip| {
            oxiroute_import::haproxy::import_roots_with_environment(
                paths,
                oxiroute_import::haproxy::PreprocessingEnvironment {
                    node_ip,
                    gpu1_defined,
                },
            )
        },
    );
    match output {
        ImportOutput::Preview => preview_with_shadow_listener_offset(
            report.value().validated(),
            shadow_port_offset,
            format,
        ),
        ImportOutput::Report => {
            if shadow_port_offset.is_some() {
                return Err("--shadow-port-offset requires --output preview".into());
            }
            Ok(
                oxiroute_import::ImportReportEnvelope::from_haproxy(&report, paths)
                    .to_json_line()?,
            )
        }
    }
}

fn import_squid(
    path: &Path,
    shadow_port_offset: Option<u16>,
    format: ConfigFormat,
    output: ImportOutput,
) -> Result<String, Box<dyn Error>> {
    let report = oxiroute_import::squid::import(path);
    match output {
        ImportOutput::Preview => preview_with_shadow_listener_offset(
            report.candidate.validated(),
            shadow_port_offset,
            format,
        ),
        ImportOutput::Report => {
            if shadow_port_offset.is_some() {
                return Err("--shadow-port-offset requires --output preview".into());
            }
            Ok(oxiroute_import::ImportReportEnvelope::from_squid(&report).to_json_line()?)
        }
    }
}

fn import_apache(
    path: &Path,
    shadow_port_offset: Option<u16>,
    format: ConfigFormat,
    output: ImportOutput,
) -> Result<String, Box<dyn Error>> {
    let report = oxiroute_import::apache::import_root(path);
    match output {
        ImportOutput::Preview => preview_with_shadow_listener_offset(
            report.candidate.validated(),
            shadow_port_offset,
            format,
        ),
        ImportOutput::Report => {
            if shadow_port_offset.is_some() {
                return Err("--shadow-port-offset requires --output preview".into());
            }
            Ok(oxiroute_import::ImportReportEnvelope::from_apache(&report).to_json_line()?)
        }
    }
}

fn import_varnish(
    path: &Path,
    arguments: &[String],
    shadow_port_offset: Option<u16>,
    format: ConfigFormat,
    output: ImportOutput,
) -> Result<String, Box<dyn Error>> {
    let invocation = oxiroute_import::varnish::VarnishdInvocation::new(arguments.iter().cloned());
    let report = oxiroute_import::varnish::import(path, &invocation);
    match output {
        ImportOutput::Preview => preview_with_shadow_listener_offset(
            report.candidate.validated(),
            shadow_port_offset,
            format,
        ),
        ImportOutput::Report => {
            if shadow_port_offset.is_some() {
                return Err("--shadow-port-offset requires --output preview".into());
            }
            Ok(oxiroute_import::ImportReportEnvelope::from_varnish(&report).to_json_line()?)
        }
    }
}

fn preview_with_shadow_listener_offset(
    config: Option<&oxiroute_config::ValidatedConfig>,
    shadow_port_offset: Option<u16>,
    format: ConfigFormat,
) -> Result<String, Box<dyn Error>> {
    let Some(offset) = shadow_port_offset else {
        return preview(config, format);
    };
    let mut config = config
        .ok_or("native configuration did not produce an activatable candidate")?
        .to_draft();
    for listener in &mut config.listeners {
        let oxiroute_config::ListenerBind::Socket { address } = &mut listener.bind else {
            continue;
        };
        address.set_port(
            address
                .port()
                .checked_add(offset)
                .ok_or("shadow listener port offset exceeds 65535")?,
        );
    }
    let config = config
        .validate()
        .map_err(|_| "shadow listener configuration is not valid")?;
    Ok(render_config(format, &config)?)
}

pub(super) fn preview(
    config: Option<&oxiroute_config::ValidatedConfig>,
    format: ConfigFormat,
) -> Result<String, Box<dyn Error>> {
    let config = config.ok_or("native configuration did not produce an activatable candidate")?;
    Ok(render_config(format, config)?)
}
