use std::error::Error;

use oxiroute_config::{ListenerBind, ValidatedConfig};
use oxiroute_config_source::render_config;
use oxiroute_import::{
    ImportReportEnvelope, apache, haproxy,
    nginx::{
        self, NginxDefaultAccessLogOverlay, NginxDefaultErrorPageOverlay, NginxHostTimezoneOverlay,
        NginxImportOptions, NginxRecordingRootOverlay,
    },
    squid, varnish,
};

use super::args::{Emit, ImportCommand, ImportOutput};
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
    render,
};

impl Execute for ImportCommand {
    fn run(&self, _: &Context<'_>) -> Result<(), CliError> {
        render::emit(&self.render().map_err(CliError::startup)?);
        Ok(())
    }
}

impl ImportCommand {
    pub(super) fn render(&self) -> Result<String, Box<dyn Error>> {
        match self {
            Self::Nginx {
                config,
                root_prefix,
                host_timezone,
                default_access_log_file,
                bearer_token_file,
                upstream_tls,
                recording_root,
                default_error_server,
                x_accel_controls_absent,
                emit,
            } => {
                let options = NginxImportOptions {
                    host_timezones: host_timezone
                        .iter()
                        .map(|timezone| NginxHostTimezoneOverlay {
                            timezone: timezone.clone(),
                        })
                        .collect(),
                    default_access_log: default_access_log_file
                        .as_ref()
                        .map(|path| NginxDefaultAccessLogOverlay { path: path.clone() }),
                    recording_root: recording_root
                        .as_ref()
                        .map(|path| NginxRecordingRootOverlay { path: path.clone() }),
                    default_error_page: default_error_server.as_ref().map(|server| {
                        NginxDefaultErrorPageOverlay {
                            server: server.clone(),
                        }
                    }),
                    x_accel_controls_absent: *x_accel_controls_absent,
                    bearer_tokens: bearer_token_file.clone(),
                    upstream_tls: upstream_tls.clone(),
                };
                let report = nginx::import_root_with_options(config, root_prefix, &options);
                emit.finish(report.candidate.validated(), || {
                    ImportReportEnvelope::from_nginx(&report).to_json_line()
                })
            }
            Self::Haproxy {
                configs,
                node_ip,
                gpu1_defined,
                emit,
            } => {
                let report = node_ip.map_or_else(
                    || haproxy::import_roots(configs),
                    |node_ip| {
                        haproxy::import_roots_with_environment(
                            configs,
                            haproxy::PreprocessingEnvironment {
                                node_ip,
                                gpu1_defined: *gpu1_defined,
                            },
                        )
                    },
                );
                emit.finish(report.value().validated(), || {
                    ImportReportEnvelope::from_haproxy(&report, configs).to_json_line()
                })
            }
            Self::Squid { config, emit } => {
                let report = squid::import(config);
                emit.finish(report.candidate.validated(), || {
                    ImportReportEnvelope::from_squid(&report).to_json_line()
                })
            }
            Self::Apache { config, emit } => {
                let report = apache::import_root(config);
                emit.finish(report.candidate.validated(), || {
                    ImportReportEnvelope::from_apache(&report).to_json_line()
                })
            }
            Self::Varnish {
                config,
                arguments,
                emit,
            } => {
                let invocation = varnish::VarnishdInvocation::new(arguments.iter().cloned());
                let report = varnish::import(config, &invocation);
                emit.finish(report.candidate.validated(), || {
                    ImportReportEnvelope::from_varnish(&report).to_json_line()
                })
            }
        }
    }
}

impl Emit {
    pub(super) fn finish(
        &self,
        candidate: Option<&ValidatedConfig>,
        report: impl FnOnce() -> Result<String, serde_json::Error>,
    ) -> Result<String, Box<dyn Error>> {
        match self.output {
            ImportOutput::Preview => self.preview(candidate),
            ImportOutput::Report if self.shadow_port_offset.is_some() => {
                Err("--shadow-port-offset requires --output preview".into())
            }
            ImportOutput::Report => Ok(report()?),
        }
    }

    fn preview(&self, candidate: Option<&ValidatedConfig>) -> Result<String, Box<dyn Error>> {
        let candidate =
            candidate.ok_or("native configuration did not produce an activatable candidate")?;
        let Some(offset) = self.shadow_port_offset else {
            return Ok(render_config(self.format.into(), candidate)?);
        };
        let mut draft = candidate.to_draft();
        for listener in &mut draft.listeners {
            let ListenerBind::Socket { address, .. } = &mut listener.bind else {
                continue;
            };
            address.set_port(
                address
                    .port()
                    .checked_add(offset)
                    .ok_or("shadow listener port offset exceeds 65535")?,
            );
        }
        let shadowed = draft
            .validate()
            .map_err(|_| "shadow listener configuration is not valid")?;
        Ok(render_config(self.format.into(), &shadowed)?)
    }
}
