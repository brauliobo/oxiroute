use std::time::Duration;

use serde_json::{Value, json};

use super::{
    EventsCommand,
    sse::{Frame, frames},
};
use crate::cli::{
    context::{Context, Execute},
    cursor::{self, PAGE_LIMIT},
    error::CliError,
    global::Output,
    interrupt,
    net::{BodyStream, Client},
    view::View,
    wait::{self, Attempt},
};

const HEARTBEAT: Duration = Duration::from_secs(15);
const SILENCE_LIMIT: Duration = Duration::from_secs(3 * HEARTBEAT.as_secs());

impl Execute for EventsCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::List { after, limit } => context.show(&page(*after, *limit), View::Events),
            Self::Follow {
                after,
                since,
                count,
            } => {
                interrupt::exit_when_signalled()?;
                let client = context.client()?;
                let cursor = match (after, since) {
                    (Some(after), _) => Some(*after),
                    (None, Some(since)) => Some(cursor_since(&client, *since)?),
                    (None, None) => None,
                };
                Follow {
                    context,
                    client,
                    cursor,
                    remaining: *count,
                }
                .run()
            }
        }
    }
}

fn page(after: u64, limit: u16) -> String {
    format!("/api/v2/events?after={after}&limit={limit}")
}

fn cursor_since(client: &Client, since: Duration) -> Result<u64, CliError> {
    let recent = cursor::recent(client, "events", since, |after| page(after, PAGE_LIMIT))?;
    Ok(recent
        .items
        .first()
        .and_then(|event| event.get("cursor")?.as_u64())
        .map_or(recent.cursor, |first| first.saturating_sub(1)))
}

enum Pump {
    Finished,
    Dropped,
}

struct Follow<'a> {
    context: &'a Context<'a>,
    client: Client,
    cursor: Option<u64>,
    remaining: Option<u64>,
}

impl Follow<'_> {
    fn run(mut self) -> Result<(), CliError> {
        let mut stream = self.open()?;
        loop {
            match self.pump(stream)? {
                Pump::Finished => return Ok(()),
                Pump::Dropped => stream = self.reopen()?,
            }
        }
    }

    fn open(&self) -> Result<BodyStream, CliError> {
        let path = self.cursor.map_or_else(
            || "/api/v2/events/stream".to_owned(),
            |cursor| format!("/api/v2/events/stream?after={cursor}"),
        );
        self.client
            .stream(&path, "text/event-stream", &[], SILENCE_LIMIT)
    }

    fn reopen(&self) -> Result<BodyStream, CliError> {
        wait::until(
            "event stream reconnect",
            self.context.timeouts()?,
            || match self.open() {
                Ok(stream) => Ok(Attempt::Done(stream)),
                Err(error @ (CliError::Connect(_) | CliError::Timeout(_))) => {
                    Ok(Attempt::Pending(error.to_string()))
                }
                Err(error) => Err(error),
            },
        )
    }

    fn pump(&mut self, stream: BodyStream) -> Result<Pump, CliError> {
        for frame in frames(stream.lines()) {
            let Ok(frame) = frame else {
                return Ok(Pump::Dropped);
            };
            match frame.name.as_str() {
                "ready" => self.cursor = self.cursor.or_else(|| cursor_of(&frame)),
                "shutdown" => return Ok(Pump::Dropped),
                "resync_required" => {
                    self.resync(&frame);
                    return Ok(Pump::Dropped);
                }
                _ if self.deliver(&frame)? => return Ok(Pump::Finished),
                _ => {}
            }
        }
        Ok(Pump::Dropped)
    }

    fn resync(&mut self, frame: &Frame) {
        let state = serde_json::from_str::<Value>(&frame.data).unwrap_or(Value::Null);
        let oldest = state.get("oldestCursor").and_then(Value::as_u64);
        let latest = state.get("latestCursor").and_then(Value::as_u64);
        self.cursor = oldest.map(|oldest| oldest.saturating_sub(1)).or(latest);
        crate::cli::render::diagnose(&format!(
            "oxiroute: events were lost: the daemon no longer retains cursor {}; continuing from {}",
            state
                .get("cursor")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            self.cursor.unwrap_or_default()
        ));
    }

    fn deliver(&mut self, frame: &Frame) -> Result<bool, CliError> {
        let event = serde_json::from_str::<Value>(&frame.data).map_err(|_| {
            CliError::Remote("the event stream sent an event that is not JSON".into())
        })?;
        self.cursor = frame.id.or(self.cursor);
        match self.context.output() {
            Output::Json => self.context.print(&event, View::Generic)?,
            _ => self
                .context
                .print_stream(&json!({ "events": [event] }), View::Events)?,
        }
        self.remaining = self.remaining.map(|left| left - 1);
        Ok(self.remaining == Some(0))
    }
}

fn cursor_of(frame: &Frame) -> Option<u64> {
    serde_json::from_str::<Value>(&frame.data)
        .ok()?
        .get("cursor")?
        .as_u64()
}
