use serde_json::json;

use super::args::{RecorderCommand, RelayCommand, RtmpCommand, StreamCommand};
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
};

const NO_CANCELLATION_HANDLE: &str =
    "the active RTMP registry does not own a safe cancellation handle for this operation";

impl Execute for RtmpCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::Stream { command } => command.run(context),
            Self::Recorder { command } => command.run(context),
            Self::Relay { command } => command.run(context),
        }
    }
}

impl Execute for StreamCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::List => context.show("/api/v1/rtmp/streams"),
            Self::Show { stream } => context.show(&format!("/api/v1/rtmp/streams/{stream}")),
            Self::Disconnect { .. } => Err(CliError::Unsupported(NO_CANCELLATION_HANDLE)),
        }
    }
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
        context.print_response(&response)
    }
}

impl Execute for RelayCommand {
    fn run(&self, _: &Context<'_>) -> Result<(), CliError> {
        Err(CliError::Unsupported(NO_CANCELLATION_HANDLE))
    }
}
