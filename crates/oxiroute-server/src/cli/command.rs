use std::process::ExitCode;

use clap::Subcommand;

use super::{
    check::{self, CheckArgs},
    completions::CompletionsArgs,
    config::ConfigCommand,
    context::{Context, Execute},
    error::CliError,
    events::EventsCommand,
    generation::GenerationCommand,
    import::ImportCommand,
    listener::ListenerCommand,
    man::ManArgs,
    pool::PoolCommand,
    process::ProcessCommand,
    registry::RegistryCommand,
    requests::RequestsCommand,
    rtmp::RtmpCommand,
    serve::ServeArgs,
    server::ServerCommand,
    tls::TlsCommand,
    top::TopArgs,
    version::VersionArgs,
};

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Run the proxy and reconcile changes to its canonical configuration.
    Serve(ServeArgs),
    #[command(flatten)]
    Process(ProcessCommand),
    Events {
        #[command(subcommand)]
        command: EventsCommand,
    },
    /// Print one monitoring line and exit with a Nagios status.
    ///
    /// Reads the public status page, over the management endpoint or a read-only status socket
    /// (`--socket`), and needs no token. Exit codes: 0 OK, 1 WARNING, 2 CRITICAL, 3 UNKNOWN (the
    /// endpoint cannot be reached or understood, or the options are wrong).
    ///
    /// This is the one command that does not use the exit categories 2 to 9 of the other
    /// commands: a monitoring system reads 2 as CRITICAL and 3 as UNKNOWN.
    Check(CheckArgs),
    /// Live read-only dashboard of listeners, pools, servers and in-flight requests.
    ///
    /// Reads the management API with the management token. Without a terminal, or with `--once`
    /// or `--json`, it prints one snapshot and exits. Keys: Tab or 1-4 switch view, `<` `>` pick the
    /// sort column, r reverses it, / filters by name, p pauses, j k select, q quits.
    Top(TopArgs),
    /// Inspect the requests currently inside the router.
    Requests {
        #[command(subcommand)]
        command: RequestsCommand,
    },
    Server {
        #[command(subcommand)]
        command: ServerCommand,
    },
    Pool {
        #[command(subcommand)]
        command: PoolCommand,
    },
    Listener {
        #[command(subcommand)]
        command: ListenerCommand,
    },
    Generation {
        #[command(subcommand)]
        command: GenerationCommand,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Tls {
        #[command(subcommand)]
        command: TlsCommand,
    },
    Rtmp {
        #[command(subcommand)]
        command: RtmpCommand,
    },
    /// Manage the runtime server registry.
    Registry {
        #[command(subcommand)]
        command: RegistryCommand,
    },
    /// Import a native configuration.
    Import {
        #[command(subcommand)]
        command: ImportCommand,
    },
    /// Print the build version.
    Version(VersionArgs),
    /// Print a shell completion script for oxiroute and oxr.
    Completions(CompletionsArgs),
    /// Print the manual page.
    Man(ManArgs),
}

impl Command {
    pub(super) fn run(&self, context: &Context<'_>) -> Result<ExitCode, CliError> {
        match self {
            Self::Check(args) => return Ok(check::run(context, args)),
            Self::Serve(args) => args.run(context),
            Self::Process(command) => command.run(context),
            Self::Events { command } => command.run(context),
            Self::Top(args) => args.run(context),
            Self::Requests { command } => command.run(context),
            Self::Server { command } => command.run(context),
            Self::Pool { command } => command.run(context),
            Self::Listener { command } => command.run(context),
            Self::Generation { command } => command.run(context),
            Self::Config { command } => command.run(context),
            Self::Tls { command } => command.run(context),
            Self::Rtmp { command } => command.run(context),
            Self::Registry { command } => command.run(context),
            Self::Import { command } => command.run(context),
            Self::Version(args) => args.run(context),
            Self::Completions(args) => args.run(context),
            Self::Man(args) => args.run(context),
        }
        .map(|()| ExitCode::SUCCESS)
    }
}
