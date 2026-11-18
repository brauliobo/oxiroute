use serde_json::json;

use super::{
    args::{RecorderCommand, RelayCommand, RtmpCommand, StreamCommand},
    session::{Role, kick},
};
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
    view::View,
};

const NO_CANCELLATION_HANDLE: &str =
    "the active RTMP registry does not own a safe cancellation handle for this operation";

impl Execute for RtmpCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::Stream { command } => command.run(context),
            Self::Publisher { command } => command.run(context, Role::Publisher),
            Self::Viewer { command } => command.run(context, Role::Subscriber),
            Self::Recorder { command } => command.run(context),
            Self::Relay { command } => command.run(context),
            Self::Recording { command } => command.run(context),
        }
    }
}

impl Execute for StreamCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::List => context.show("/api/v1/rtmp/streams", View::RtmpStreams),
            Self::Show { stream } => {
                context.show(&format!("/api/v1/rtmp/streams/{stream}"), View::RtmpStream)
            }
            Self::Disconnect { stream } => disconnect(context, stream),
        }
    }
}

fn disconnect(context: &Context<'_>, stream: &str) -> Result<(), CliError> {
    let value = context
        .client()?
        .get(&format!("/api/v1/rtmp/streams/{stream}"), true)?
        .json()?;
    let publisher = value["publisher"]["session_id"].as_str().ok_or_else(|| {
        CliError::Status(
            409,
            "no_publisher: the stream has no publisher to disconnect".into(),
        )
    })?;
    kick(context, Role::Publisher, publisher)
}

impl Execute for RecorderCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        let (stream, recorder, action) = match self {
            Self::Start { stream, recorder } => (stream, recorder, "start"),
            Self::Stop { stream, recorder } => (stream, recorder, "stop"),
        };
        let path = format!("/api/v1/rtmp/streams/{stream}/recorders/{recorder}/{action}");
        let response = context
            .client()?
            .generation_json("POST", &path, &json!({}))?;
        context.print_response(&response, View::Generic)
    }
}

impl Execute for RelayCommand {
    fn run(&self, _: &Context<'_>) -> Result<(), CliError> {
        Err(CliError::Unsupported(NO_CANCELLATION_HANDLE))
    }
}
