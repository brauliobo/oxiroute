use clap::{Args, CommandFactory as _, ValueEnum};
use clap_complete::Shell;

use super::{
    Cli,
    binary::Binary,
    context::{Context, Execute},
    error::CliError,
    render,
};

#[derive(Clone, Copy, ValueEnum)]
enum Supported {
    Bash,
    Zsh,
    Fish,
}

impl From<Supported> for Shell {
    fn from(shell: Supported) -> Self {
        match shell {
            Supported::Bash => Self::Bash,
            Supported::Zsh => Self::Zsh,
            Supported::Fish => Self::Fish,
        }
    }
}

#[derive(Args)]
pub(crate) struct CompletionsArgs {
    /// Shell to write the completion script for.
    #[arg(value_enum)]
    shell: Supported,
    /// Command name the script completes: `oxiroute` or its short alias `oxr`.
    #[arg(long, value_enum, default_value_t = Binary::Oxiroute)]
    name: Binary,
}

impl CompletionsArgs {
    fn script(&self) -> Vec<u8> {
        let mut script = Vec::new();
        clap_complete::generate(
            Shell::from(self.shell),
            &mut Cli::command(),
            self.name.as_str(),
            &mut script,
        );
        script
    }
}

impl Execute for CompletionsArgs {
    fn run(&self, _: &Context<'_>) -> Result<(), CliError> {
        render::emit_bytes(&self.script())
    }
}

#[cfg(test)]
mod tests;
