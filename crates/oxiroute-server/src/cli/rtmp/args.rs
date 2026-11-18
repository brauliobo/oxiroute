use clap::Subcommand;

use crate::cli::parse::nonempty_target;

#[derive(Subcommand)]
pub(crate) enum RtmpCommand {
    Stream {
        #[command(subcommand)]
        command: StreamCommand,
    },
    Recorder {
        #[command(subcommand)]
        command: RecorderCommand,
    },
    Relay {
        #[command(subcommand)]
        command: RelayCommand,
    },
}

#[derive(Subcommand)]
pub(crate) enum StreamCommand {
    List,
    Show {
        #[arg(value_parser = nonempty_target)]
        stream: String,
    },
    Disconnect {
        #[arg(value_parser = nonempty_target)]
        stream: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum RecorderCommand {
    Start {
        #[arg(value_parser = nonempty_target)]
        stream: String,
        #[arg(value_parser = nonempty_target)]
        recorder: String,
    },
    Stop {
        #[arg(value_parser = nonempty_target)]
        stream: String,
        #[arg(value_parser = nonempty_target)]
        recorder: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum RelayCommand {
    Reconnect {
        #[arg(value_parser = nonempty_target)]
        stream: String,
        #[arg(value_parser = nonempty_target)]
        relay: String,
    },
}
