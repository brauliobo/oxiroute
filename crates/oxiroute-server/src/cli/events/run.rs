use std::{thread, time::Duration};

use serde_json::Value;

use super::EventsCommand;
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
};

impl Execute for EventsCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::List { after, limit } => context.show(&path(*after, *limit)),
            Self::Follow {
                after,
                limit,
                interval_ms,
            } => follow(context, *after, *limit, *interval_ms),
        }
    }
}

fn path(after: u64, limit: u16) -> String {
    format!("/api/v1/events?after={after}&limit={limit}")
}

fn follow(context: &Context<'_>, after: u64, limit: u16, interval_ms: u64) -> Result<(), CliError> {
    let client = context.client()?;
    let mut cursor = after;
    loop {
        let value = client.get(&path(cursor, limit), true)?.json()?;
        if let Some(events) = value.get("events").and_then(Value::as_array) {
            for event in events {
                context.print(event)?;
            }
        }
        cursor = value
            .get("cursor")
            .and_then(Value::as_u64)
            .unwrap_or(cursor);
        thread::sleep(Duration::from_millis(interval_ms));
    }
}
