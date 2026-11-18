use std::path::PathBuf;

use clap::Args;

use super::{
    context::{Context, Execute},
    error::CliError,
};

const DEFAULT_CONFIG: &str = "oxiroute.kdl";

#[derive(Args)]
pub(crate) struct ServeArgs {
    #[arg(value_name = "CONFIG", default_value = DEFAULT_CONFIG)]
    pub(super) config: PathBuf,
}

impl ServeArgs {
    pub(super) fn new(config: Option<PathBuf>) -> Self {
        Self {
            config: config.unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG)),
        }
    }
}

impl Execute for ServeArgs {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        context.serve(&self.config)
    }
}
