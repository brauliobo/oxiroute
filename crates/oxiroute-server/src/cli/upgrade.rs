use std::path::PathBuf;

use clap::Args;

use super::{
    context::{Context, Execute},
    error::CliError,
};

const DEFAULT_CONFIG: &str = "/etc/oxiroute/oxiroute.kdl";

/// Hand the listeners over to a new executable without dropping a connection.
///
/// The running master starts the executable as its successor, which takes the listening sockets and
/// serves them with its own workers. The workers of the running master finish the connections they
/// hold. When the successor does not become ready, the running master keeps serving and this
/// command fails with the reason. Needs the supervised runtime; a daemon that runs without the
/// supervising master has nothing to hand over from.
#[derive(Args)]
pub(crate) struct UpgradeArgs {
    /// Configuration file the master to upgrade runs with.
    #[arg(long, value_name = "CONFIG", default_value = DEFAULT_CONFIG)]
    pub(super) config: PathBuf,
    /// Executable to hand over to; the one the master started from when not given.
    #[arg(long, value_name = "PATH")]
    pub(super) binary: Option<PathBuf>,
    /// Return when the previous worker has finished its connections, not when the successor serves.
    #[arg(long)]
    pub(super) wait: bool,
}

#[cfg(target_os = "linux")]
impl Execute for UpgradeArgs {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        use super::{global::Output, render};
        use crate::upgrade::{Command, UpgradeError, request};

        let binary = self
            .binary
            .as_deref()
            .map(std::fs::canonicalize)
            .transpose()
            .map_err(|error| CliError::local(format!("--binary: {error}")))?;
        let json = matches!(context.output(), Output::Json);
        let mut written = Ok(());
        let outcome = request(&self.config, &Command { binary }, self.wait, |event| {
            if written.is_ok() && !context.quiet() {
                written = render::emit(&progress(event, json));
            }
        });
        written?;
        outcome.map_err(|error| match error {
            UpgradeError::Config { .. } => CliError::local(error),
            UpgradeError::NotRunning { .. } | UpgradeError::Io(_) => {
                CliError::Connect(error.to_string())
            }
            UpgradeError::Refused { .. }
            | UpgradeError::Interrupted
            | UpgradeError::Failed { .. } => CliError::Remote(error.to_string()),
        })
    }
}

#[cfg(target_os = "linux")]
fn progress(event: &crate::upgrade::Event, json: bool) -> String {
    use crate::upgrade::Event;

    if json {
        return format!("{}\n", serde_json::to_string(event).unwrap_or_default());
    }
    match event {
        Event::Starting {
            binary,
            build_id,
            previous_build_id,
        } => format!(
            "starting {} (build {build_id}, replacing {previous_build_id})\n",
            binary.display()
        ),
        Event::Serving { pid } => {
            format!("serving: the successor master {pid} holds the listeners\n")
        }
        Event::Drained => "drained: the previous worker has finished its connections\n".into(),
        Event::Refused | Event::Failed { .. } => String::new(),
    }
}

#[cfg(not(target_os = "linux"))]
impl Execute for UpgradeArgs {
    fn run(&self, _: &Context<'_>) -> Result<(), CliError> {
        Err(CliError::Unsupported(
            "binary hand-over needs the supervised runtime, which is Linux only",
        ))
    }
}
