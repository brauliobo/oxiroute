use std::{net::IpAddr, path::PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use oxiroute_import::nginx::{NginxBearerTokenOverlay, NginxUpstreamTlsOverlay};

use super::overlay::{parse_bearer_token_file, parse_upstream_tls};
use crate::cli::config::ComposeFormat;

#[derive(Subcommand)]
pub(crate) enum ImportCommand {
    /// Import an nginx configuration.
    Nginx {
        /// Main nginx configuration file.
        #[arg(value_name = "CONFIG")]
        config: PathBuf,
        /// Directory the configuration's absolute paths are resolved under.
        #[arg(long, value_name = "DIRECTORY", default_value = "/")]
        root_prefix: PathBuf,
        /// Preserve native local-time recording suffixes with this IANA timezone.
        #[arg(long, value_name = "IANA_TIMEZONE")]
        host_timezone: Option<String>,
        /// Explicitly migrate omitted AND formatted nginx access logs to `OxiRoute` structured JSONL.
        #[arg(long, value_name = "FILE")]
        default_access_log_file: Option<PathBuf>,
        /// Serve a literal-bearer `if` gate from this 0400/0600 token file (repeatable).
        #[arg(
            long,
            value_name = "SERVER_NAME=ABS_PATH",
            value_parser = parse_bearer_token_file
        )]
        bearer_token_file: Vec<NginxBearerTokenOverlay>,
        /// Verify one https upstream authority with this SNI, optional CA, and optional activation (repeatable).
        #[arg(
            long,
            value_name = "AUTHORITY=SNI[,ca=PATH][,activate]",
            value_parser = parse_upstream_tls
        )]
        upstream_tls: Vec<NginxUpstreamTlsOverlay>,
        /// Replace one unique native recording root with this no-symlink canonical path.
        #[arg(long, value_name = "DIRECTORY")]
        recording_root: Option<PathBuf>,
        /// Preserve nginx's branded default static 404 body and response header.
        #[arg(long, value_name = "SERVER_TOKEN")]
        default_error_server: Option<String>,
        /// Assert that audited upstreams do not emit nginx X-Accel response controls.
        #[arg(long)]
        x_accel_controls_absent: bool,
        #[command(flatten)]
        options: ImportOptions,
    },
    /// Import `haproxy` configuration files.
    Haproxy {
        /// Configuration files, in the order `haproxy` loads them.
        #[arg(value_name = "CONFIG", required = true)]
        configs: Vec<PathBuf>,
        /// Expand `${NODE_IP}` using this exact host address.
        #[arg(long, value_name = "IP")]
        node_ip: Option<IpAddr>,
        /// Treat `defined(GPU1)` conditions as true.
        #[arg(long, requires = "node_ip")]
        gpu1_defined: bool,
        #[command(flatten)]
        options: ImportOptions,
    },
    /// Import a Squid configuration.
    Squid {
        /// Squid configuration file.
        #[arg(value_name = "CONFIG")]
        config: PathBuf,
        #[command(flatten)]
        options: ImportOptions,
    },
    /// Import an Apache httpd configuration.
    Apache {
        /// Apache configuration file.
        #[arg(value_name = "CONFIG")]
        config: PathBuf,
        #[command(flatten)]
        options: ImportOptions,
    },
    /// Import a Varnish VCL file.
    Varnish {
        /// VCL file.
        #[arg(value_name = "CONFIG")]
        config: PathBuf,
        /// Add one exact varnishd argument used to interpret the VCL source.
        #[arg(
            long = "arg",
            value_name = "ARG",
            action = clap::ArgAction::Append,
            allow_hyphen_values = true
        )]
        arguments: Vec<String>,
        #[command(flatten)]
        options: ImportOptions,
    },
}

#[derive(Args)]
pub(crate) struct ImportOptions {
    /// Shift imported IP socket listener ports for side-by-side validation.
    #[arg(long, value_name = "PORTS", value_parser = clap::value_parser!(u16).range(1..))]
    pub(super) shadow_port_offset: Option<u16>,
    /// Render preview output in this canonical configuration format.
    #[arg(long, value_enum, default_value_t = ComposeFormat::Kdl)]
    pub(super) format: ComposeFormat,
    /// Print the importer's evidence report, or a finalized canonical configuration preview.
    #[arg(long, value_enum, default_value_t = ImportEmit::Report)]
    pub(super) emit: ImportEmit,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum ImportEmit {
    #[default]
    Report,
    Preview,
}
