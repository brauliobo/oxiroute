use std::path::PathBuf;

use clap::Args;

use super::{
    context::{Context, Execute},
    error::CliError,
    fs::ensure_readable_config,
};

const DEFAULT_CONFIG: &str = "oxiroute.kdl";

#[derive(Args)]
pub(crate) struct ServeArgs {
    /// Configuration file to serve.
    #[arg(value_name = "CONFIG", default_value = DEFAULT_CONFIG)]
    pub(super) config: PathBuf,
}

impl Execute for ServeArgs {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        ensure_readable_config(&self.config)?;
        context.serve(&self.config)
    }
}
