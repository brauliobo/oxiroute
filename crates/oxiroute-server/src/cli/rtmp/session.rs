use serde_json::{Value, json};

use super::args::SessionCommand;
use crate::cli::{context::Context, error::CliError, view::View};

const CLIENTS: &str = "/api/v1/rtmp/stats/clients";
const REVISION: &str = "If-Rtmp-Session-Revision";

#[derive(Clone, Copy)]
pub(super) enum Role {
    Publisher,
    Subscriber,
}

impl Role {
    const fn name(self) -> &'static str {
        match self {
            Self::Publisher => "publisher",
            Self::Subscriber => "subscriber",
        }
    }
}

impl SessionCommand {
    pub(super) fn run(&self, context: &Context<'_>, role: Role) -> Result<(), CliError> {
        match self {
            Self::List => list(context, role),
            Self::Kick { session } => kick(context, role, session),
        }
    }
}

fn clients(context: &Context<'_>) -> Result<Value, CliError> {
    context.client()?.get(CLIENTS, true)?.json()
}

fn list(context: &Context<'_>, role: Role) -> Result<(), CliError> {
    let mut value = clients(context)?;
    if let Some(sessions) = value.get_mut("clients").and_then(Value::as_array_mut) {
        sessions.retain(|client| client.get("role").and_then(Value::as_str) == Some(role.name()));
    }
    context.print(&value, View::RtmpClients)
}

pub(super) fn kick(context: &Context<'_>, role: Role, session: &str) -> Result<(), CliError> {
    let value = clients(context)?;
    let revision = value["clients"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|client| client["id"] == session)
        .and_then(|client| client["revision"].as_str())
        .ok_or_else(|| {
            CliError::Status(
                404,
                "session_not_found: the session is not connected".into(),
            )
        })?;
    let path = format!("/api/v1/rtmp/clients/{session}/{}/drop", role.name());
    let response = context
        .client()?
        .json("POST", &path, &json!({}), &[(REVISION, revision)])?;
    context.print_response(&response, View::Generic)
}
