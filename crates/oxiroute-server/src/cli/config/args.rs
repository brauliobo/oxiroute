use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use oxiroute_config_source::ConfigFormat;

use super::input::InputFormat;

#[derive(Subcommand)]
pub(crate) enum ConfigCommand {
    /// Check a configuration file offline: parse, validate and prepare it without a daemon.
    Check {
        /// Configuration file in a native or canonical format.
        config: PathBuf,
    },
    /// Compose finalized canonical configurations in the supplied order.
    ///
    /// The output is rendered from the typed configuration: comments, formatting and Lua logic of
    /// the inputs are not kept.
    Compose {
        /// Format to print the composed configuration in.
        #[arg(long, value_enum, default_value_t = ComposeFormat::Kdl)]
        format: ComposeFormat,
        /// Finalized canonical configuration files, in the order they apply.
        #[arg(value_name = "CONFIG", required = true, num_args = 1..)]
        configs: Vec<PathBuf>,
    },
    /// Show the active configuration's format, revisions and number of entries per section.
    Get,
    /// Print the authored configuration text: the file the daemon runs, or a file on disk.
    Show(ShowArgs),
    /// Ask the daemon whether a configuration file is valid, without publishing it.
    Validate(SourceArgs),
    /// Show which settings a configuration file changes against the daemon's configuration.
    Diff(DiffArgs),
    /// Replace the daemon's configuration file with a configuration file and reload it.
    Apply(ApplyArgs),
    /// Edit the daemon's configuration text in $VISUAL or $EDITOR, then validate and apply it.
    Edit(EditArgs),
    /// Rewrite a configuration file in its canonical form.
    Fmt(FmtArgs),
}

#[derive(Args)]
pub(crate) struct SourceArgs {
    /// Configuration file in KDL, Lua, HOCON, UCI or JSON, chosen by its extension.
    pub(super) file: PathBuf,
    /// Format of the file when its extension does not say.
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub(super) format: Option<InputFormat>,
}

#[derive(Args)]
pub(crate) struct ShowArgs {
    /// Print this file instead of asking the daemon; no daemon is needed.
    #[arg(conflicts_with_all = ["active", "disk"])]
    pub(super) file: Option<PathBuf>,
    /// Print the running configuration, rendered canonically (comments do not exist there).
    #[arg(long, conflicts_with = "disk")]
    pub(super) active: bool,
    /// Print the authored text stored on disk (the default).
    #[arg(long)]
    pub(super) disk: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum Against {
    Active,
    Disk,
}

#[derive(Args)]
pub(crate) struct DiffArgs {
    #[command(flatten)]
    pub(super) source: SourceArgs,
    /// Compare with the running configuration or with the one stored on disk.
    #[arg(long, value_enum, default_value_t = Against::Active)]
    pub(super) against: Against,
    /// Also print the unified diff of the authored text against the stored file.
    #[arg(long)]
    pub(super) text: bool,
    /// Exit with 1 when there are differences, 0 when there are none.
    #[arg(long)]
    pub(super) exit_code: bool,
}

#[derive(Args)]
pub(crate) struct ApplyArgs {
    #[command(flatten)]
    pub(super) source: SourceArgs,
    /// Store the equivalent of a file in another format than the daemon's, losing its comments and logic.
    #[arg(long)]
    pub(super) convert: bool,
    /// Validate and show the changes, write nothing.
    #[arg(long)]
    pub(super) dry_run: bool,
    /// Wait until the new revision is active or rejected, up to --timeout (30s by default).
    #[arg(long)]
    pub(super) wait: bool,
}

#[derive(Args)]
pub(crate) struct EditArgs {
    /// Apply without asking for confirmation.
    #[arg(long)]
    pub(super) yes: bool,
    /// Wait until the new revision is active or rejected, up to --timeout (30s by default).
    #[arg(long)]
    pub(super) wait: bool,
}

#[derive(Args)]
pub(crate) struct FmtArgs {
    /// Configuration file in KDL, Lua, HOCON or UCI, chosen by its extension.
    pub(super) file: PathBuf,
    /// Format of the file when its extension does not say.
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub(super) format: Option<InputFormat>,
    /// Print nothing; exit with 1 when the file is not in canonical form.
    #[arg(long, conflicts_with_all = ["write", "diff"])]
    pub(super) check: bool,
    /// Rewrite the file in place.
    #[arg(long, conflicts_with = "diff")]
    pub(super) write: bool,
    /// Print the unified diff to the canonical form; exit with 1 when there is one.
    #[arg(long)]
    pub(super) diff: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum ComposeFormat {
    #[default]
    Kdl,
    Lua,
    Uci,
    Hocon,
}

impl From<ComposeFormat> for ConfigFormat {
    fn from(format: ComposeFormat) -> Self {
        match format {
            ComposeFormat::Kdl => Self::Kdl,
            ComposeFormat::Lua => Self::Lua,
            ComposeFormat::Uci => Self::Uci,
            ComposeFormat::Hocon => Self::Hocon,
        }
    }
}
