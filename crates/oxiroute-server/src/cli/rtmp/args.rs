use clap::Subcommand;

use super::recording::RecordingCommand;
use crate::cli::parse::nonempty_target;

#[derive(Subcommand)]
pub(crate) enum RtmpCommand {
    /// List and inspect the live RTMP streams, or disconnect the publisher of one.
    Stream {
        #[command(subcommand)]
        command: StreamCommand,
    },
    /// List the connected publishers and disconnect one.
    Publisher {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// List the connected viewers and disconnect one.
    Viewer {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Start and stop the recorders of a stream.
    Recorder {
        #[command(subcommand)]
        command: RecorderCommand,
    },
    /// Manage the relays of a stream.
    Relay {
        #[command(subcommand)]
        command: RelayCommand,
    },
    /// List, inspect, download and delete the stored recordings, and show their quota.
    Recording {
        #[command(subcommand)]
        command: RecordingCommand,
    },
}

#[derive(Subcommand)]
pub(crate) enum StreamCommand {
    /// List the live streams with publisher, subscribers and media volume.
    List,
    /// Show one live stream.
    Show {
        /// Id of the stream, as printed by `rtmp stream list`.
        #[arg(value_parser = nonempty_target)]
        stream: String,
    },
    /// Disconnect the publisher of a stream; the stream and its viewers stay.
    Disconnect {
        /// Id of the stream.
        #[arg(value_parser = nonempty_target)]
        stream: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum SessionCommand {
    /// List the connected sessions with their stream, peer and connection age.
    List,
    /// Disconnect one session; it may reconnect.
    Kick {
        /// Id of the session, as printed by `list`.
        #[arg(value_parser = nonempty_target)]
        session: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum RecorderCommand {
    /// Start a configured recorder on a live stream.
    Start {
        /// Id of the stream.
        #[arg(value_parser = nonempty_target)]
        stream: String,
        /// Name of the recorder in the configuration.
        #[arg(value_parser = nonempty_target)]
        recorder: String,
    },
    /// Stop a running recorder of a live stream.
    Stop {
        /// Id of the stream.
        #[arg(value_parser = nonempty_target)]
        stream: String,
        /// Name of the recorder in the configuration.
        #[arg(value_parser = nonempty_target)]
        recorder: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum RelayCommand {
    /// Reconnect a relay of a stream; not available, the daemon has no safe cancellation handle yet.
    Reconnect {
        /// Id of the stream.
        #[arg(value_parser = nonempty_target)]
        stream: String,
        /// Name of the relay.
        #[arg(value_parser = nonempty_target)]
        relay: String,
    },
}
