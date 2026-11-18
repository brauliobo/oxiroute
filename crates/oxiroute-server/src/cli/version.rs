use clap::Args;

use super::{
    BUILD_VERSION,
    context::{Context, Execute},
    error::CliError,
    render,
};

#[derive(Args)]
pub(crate) struct VersionArgs;

impl Execute for VersionArgs {
    fn run(&self, _: &Context<'_>) -> Result<(), CliError> {
        render::emit(&format!("{BUILD_VERSION}\n"));
        Ok(())
    }
}
