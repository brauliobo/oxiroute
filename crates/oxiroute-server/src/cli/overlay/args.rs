use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum OverlayCommand {
    /// List the settings kept with `--persist` and whether each one is applied.
    ///
    /// A setting is `applied` when the running daemon holds it, `diverged` when a later change
    /// replaced it until the next restart, and `orphan` when its pool, server or listener no longer
    /// exists.
    Show,
    /// Delete the overlay file. The running state does not change; later restarts start from the
    /// configuration alone.
    Clear,
    /// Write the overlay's connection limits into the configuration file, in its own format.
    ///
    /// Only settings with a configuration equivalent are promoted and removed from the overlay;
    /// states, health, checks, weights and listener states stay in it. The file is saved from its
    /// parsed form: its comments and logic are not kept, and a file composed from several is
    /// refused.
    Promote {
        /// Wait until the daemon has activated the saved configuration.
        #[arg(long)]
        wait: bool,
    },
}
