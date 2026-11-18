use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum TokenCommand {
    /// Create the management token file with a random 64-character token that only its owner can read.
    ///
    /// The file is the one `--token-file` names, else the one the CLI would read (`OXIROUTE_MANAGEMENT_TOKEN_FILE`, the
    /// packaged configuration's `token_file`), else /etc/oxiroute/management.token. An existing file is never replaced:
    /// use `token rotate`. The token itself is never printed; the CLI and the daemon read it from the file.
    Create,
    /// Replace the token in the management token file with a new random one, keeping its owner and mode.
    ///
    /// The file is swapped atomically, so a reader sees the old or the new token and never a partial one. The daemon
    /// reads the token when it starts: restart it to accept the new token, which the CLI uses at once.
    Rotate,
}
