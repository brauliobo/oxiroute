use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use oxiroute_config::{ListenerBind, ValidatedConfig};
use oxiroute_config_source::render_config;
use oxiroute_import::{ImportReportEnvelope, apache, haproxy, nginx, squid, varnish};

use super::{
    args::{ImportCommand, ImportEmit, ImportOptions},
    blockers::{self, NGINX_REMEDIATION},
};
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
    render,
};

pub(super) struct Imported {
    pub(super) text: String,
    pub(super) blocked: Option<String>,
}

impl Execute for ImportCommand {
    fn run(&self, _: &Context<'_>) -> Result<(), CliError> {
        let imported = self.render().map_err(CliError::local)?;
        if !imported.text.is_empty() {
            render::emit(&imported.text)?;
        }
        imported
            .blocked
            .map_or(Ok(()), |summary| Err(CliError::Blocked(summary)))
    }
}

fn readable(config: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let absolute = std::path::absolute(config)?;
    let metadata = fs::metadata(&absolute)
        .map_err(|error| format!("cannot read {}: {error}", config.display()))?;
    if !metadata.is_file() {
        return Err(format!("cannot read {}: not a file", config.display()).into());
    }
    Ok(absolute)
}

impl ImportCommand {
    pub(super) fn render(&self) -> Result<Imported, Box<dyn Error>> {
        match self {
            Self::Nginx {
                config,
                root_prefix,
                overlays,
                options,
            } => {
                let overlays = overlays.options();
                let config = readable(config)?;
                let report = nginx::import_root_with_options(&config, root_prefix, &overlays);
                options.finish(
                    &config.display().to_string(),
                    report.candidate.validated(),
                    &ImportReportEnvelope::from_nginx(&report),
                    Some(NGINX_REMEDIATION),
                )
            }
            Self::Haproxy {
                configs,
                node_ip,
                gpu1_defined,
                options,
            } => {
                for config in configs {
                    readable(config)?;
                }
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
                let sources = configs
                    .iter()
                    .map(|config| config.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                options.finish(
                    &sources,
                    report.value().validated(),
                    &ImportReportEnvelope::from_haproxy(&report, configs),
                    None,
                )
            }
            Self::Squid { config, options } => {
                readable(config)?;
                let report = squid::import(config);
                options.finish(
                    &config.display().to_string(),
                    report.candidate.validated(),
                    &ImportReportEnvelope::from_squid(&report),
                    None,
                )
            }
            Self::Apache { config, options } => {
                readable(config)?;
                let report = apache::import_root(config);
                options.finish(
                    &config.display().to_string(),
                    report.candidate.validated(),
                    &ImportReportEnvelope::from_apache(&report),
                    None,
                )
            }
            Self::Varnish {
                config,
                arguments,
                options,
            } => {
                let invocation = varnish::VarnishdInvocation::new(arguments.iter().cloned());
                readable(config)?;
                let report = varnish::import(config, &invocation);
                options.finish(
                    &config.display().to_string(),
                    report.candidate.validated(),
                    &ImportReportEnvelope::from_varnish(&report),
                    None,
                )
            }
        }
    }
}

impl ImportOptions {
    pub(super) fn finish(
        &self,
        source: &str,
        candidate: Option<&ValidatedConfig>,
        report: &ImportReportEnvelope,
        remediation: Option<&str>,
    ) -> Result<Imported, Box<dyn Error>> {
        let blocked = blockers::summary(source, report, remediation);
        match (self.emit, candidate) {
            (ImportEmit::Preview, Some(candidate)) => Ok(Imported {
                text: self.preview(candidate)?,
                blocked: None,
            }),
            (ImportEmit::Preview, None) => Ok(Imported {
                text: String::new(),
                blocked: Some(format!(
                    "{}\nno preview was printed; run the import with --emit report for the full evidence",
                    blocked.unwrap_or_else(|| format!(
                        "{source}: the import did not produce an activatable candidate"
                    ))
                )),
            }),
            (ImportEmit::Report, _) if self.shadow_port_offset.is_some() => {
                Err("--shadow-port-offset requires --emit preview".into())
            }
            (ImportEmit::Report, _) => Ok(Imported {
                text: report.to_json_line()?,
                blocked,
            }),
        }
    }

    fn preview(&self, candidate: &ValidatedConfig) -> Result<String, Box<dyn Error>> {
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
