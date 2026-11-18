use std::process::ExitCode;

use clap::Subcommand;

use super::{
    check::{self, CheckArgs},
    completions::CompletionsArgs,
    config::ConfigCommand,
    context::{Context, Execute},
    contexts::ContextCommand,
    error::CliError,
    events::EventsCommand,
    generation::GenerationCommand,
    help,
    import::ImportCommand,
    listener::ListenerCommand,
    man::ManArgs,
    overlay::OverlayCommand,
    pool::PoolCommand,
    process::ProcessCommand,
    registry::RegistryCommand,
    requests::RequestsCommand,
    rtmp::RtmpCommand,
    serve::ServeArgs,
    server::ServerCommand,
    tls::{self, TlsCommand},
    top::TopArgs,
    version::VersionArgs,
};

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Run the proxy and reconcile changes to its canonical configuration.
    Serve(ServeArgs),
    #[command(flatten)]
    Process(ProcessCommand),
    /// Read the management events: generation changes, administrative actions and their outcomes.
    #[command(after_help = help::EVENTS_EXAMPLES)]
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
    #[command(after_help = help::REQUESTS_EXAMPLES)]
    Requests {
        #[command(subcommand)]
        command: RequestsCommand,
    },
    /// Inspect and change servers: drain, weight, limits, health, registration.
    #[command(after_help = help::SERVER_EXAMPLES)]
    Server {
        #[command(subcommand)]
        command: ServerCommand,
    },
    /// Inspect pools and drain or evacuate all their servers at once.
    #[command(after_help = help::POOL_EXAMPLES)]
    Pool {
        #[command(subcommand)]
        command: PoolCommand,
    },
    /// Inspect listeners and stop or resume their new connections.
    #[command(after_help = help::LISTENER_EXAMPLES)]
    Listener {
        #[command(subcommand)]
        command: ListenerCommand,
    },
    /// Inspect, reload, roll back and drain configuration generations.
    #[command(after_help = help::GENERATION_EXAMPLES)]
    Generation {
        #[command(subcommand)]
        command: GenerationCommand,
    },
    /// Name daemons and their token sources, and pick the one commands talk to.
    #[command(after_help = help::CONTEXT_EXAMPLES)]
    Context {
        #[command(subcommand)]
        command: ContextCommand,
    },
    /// Show, clear or promote the settings kept with `--persist`.
    #[command(after_help = help::OVERLAY_EXAMPLES)]
    Overlay {
        #[command(subcommand)]
        command: OverlayCommand,
    },
    /// Check, compare and apply configurations.
    #[command(after_help = help::CONFIG_EXAMPLES)]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Inspect certificates and judge their expiry, reload them from disk and control ACME renewal.
    #[command(after_help = help::TLS_EXAMPLES)]
    Tls {
        #[command(subcommand)]
        command: TlsCommand,
    },
    /// Inspect RTMP streams and control their recorders and relays.
    #[command(after_help = help::RTMP_EXAMPLES)]
    Rtmp {
        #[command(subcommand)]
        command: RtmpCommand,
    },
    /// Manage the runtime server registry.
    #[command(after_help = help::REGISTRY_EXAMPLES)]
    Registry {
        #[command(subcommand)]
        command: RegistryCommand,
    },
    /// Import a native configuration.
    #[command(after_help = help::IMPORT_EXAMPLES)]
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
            Self::Tls {
                command: TlsCommand::Check(args),
            } => return Ok(tls::check(context, args)),
            Self::Serve(args) => args.run(context),
            Self::Process(command) => command.run(context),
            Self::Events { command } => command.run(context),
            Self::Top(args) => args.run(context),
            Self::Requests { command } => command.run(context),
            Self::Server { command } => command.run(context),
            Self::Overlay { command } => command.run(context),
            Self::Pool { command } => command.run(context),
            Self::Listener { command } => command.run(context),
            Self::Generation { command } => command.run(context),
            Self::Context { command } => command.run(context),
            Self::Config { command } => command.run(context),
            Self::Tls {
                command: TlsCommand::Operation(operation),
            } => operation.run(context),
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
