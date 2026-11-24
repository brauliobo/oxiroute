use clap::Args;
use serde_json::json;

use super::{
    BUILD_VERSION,
    context::{Context, Execute},
    error::CliError,
    global::Output,
    render,
};

const FORMATS: [&str; 7] = ["kdl", "oxi", "yaml", "toml", "lua", "hocon", "uci"];
const PROTOCOLS: [&str; 8] = [
    "http/1.1",
    "http/2",
    "http/3",
    "forward-http1",
    "forward-http2",
    "rtmp",
    "tcp",
    "udp",
];
const TLS_BACKENDS: [&str; 2] = ["openssl", "rustls"];
const DATA_PLANES: [&str; 2] = ["tokio", "epoll"];

#[derive(Args)]
pub(crate) struct VersionArgs {
    /// Show build target, supported formats, protocols, TLS backends, data planes and CPU SIMD level.
    #[arg(short = 'v', long)]
    pub(super) verbose: bool,
}

impl VersionArgs {
    pub(super) fn render(&self, output: Output) -> Result<String, CliError> {
        if !self.verbose {
            return Ok(format!("{BUILD_VERSION}\n"));
        }
        let target = format!("{}/{}", std::env::consts::ARCH, std::env::consts::OS);
        let cpu_level = oxiroute_cpu::Level::hardware().name();
        match output {
            Output::Json => {
                let value = json!({
                    "version": BUILD_VERSION,
                    "target": target,
                    "formats": FORMATS,
                    "protocols": PROTOCOLS,
                    "tls_backends": TLS_BACKENDS,
                    "data_planes": DATA_PLANES,
                    "cpu_level": cpu_level,
                });
                let encoded = serde_json::to_string(&value).map_err(CliError::local)?;
                Ok(format!("{encoded}\n"))
            }
            Output::Table | Output::Plain => Ok(format!(
                "version: {BUILD_VERSION}\n\
                 target: {target}\n\
                 formats: {}\n\
                 protocols: {}\n\
                 tls_backends: {}\n\
                 data_planes: {}\n\
                 cpu_level: {cpu_level}\n",
                FORMATS.join(", "),
                PROTOCOLS.join(", "),
                TLS_BACKENDS.join(", "),
                DATA_PLANES.join(", "),
            )),
        }
    }
}

impl Execute for VersionArgs {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        if self.verbose && context.quiet() {
            return Ok(());
        }
        render::emit(&self.render(context.output())?)
    }
}
