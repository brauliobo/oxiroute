use serde_json::Value;

use crate::cli::{
    error::CliError,
    net::{Client, Response, Timeouts},
    view,
    wait::{self, Attempt},
};

pub(in crate::cli) const STATUS: &str = "/api/v1/generations";

pub(in crate::cli) enum Settled {
    Activated(Response),
    Rejected(String),
}

/// The newest rejection the daemon reported before a change was requested, so that only a later one
/// counts as the answer to the change.
pub(in crate::cli) struct Baseline(Option<Value>);

impl Baseline {
    pub(in crate::cli) fn of(client: &Client) -> Result<Self, CliError> {
        let status = client.get(STATUS, true)?.json()?;
        Ok(Self(status.pointer("/generation/lastRejection").cloned()))
    }
}

/// Waits until the daemon activates `candidate` or reports a new rejection, up to the wait limit.
pub(in crate::cli) fn settle(
    client: &Client,
    timeouts: Timeouts,
    what: &str,
    candidate: Option<&Value>,
    baseline: &Baseline,
) -> Result<Settled, CliError> {
    wait::until(what, timeouts, || {
        let response = client.get(STATUS, true)?;
        let status = response.json()?;
        let generation = status.get("generation").unwrap_or(&Value::Null);
        if generation.get("activeRevision") == candidate {
            return Ok(Attempt::Done(Settled::Activated(response)));
        }
        let latest = generation
            .get("lastRejection")
            .filter(|value| value.is_object());
        if latest.is_some() && latest != baseline.0.as_ref() {
            let summary = view::rejection_summary(generation).unwrap_or_else(|| "rejected".into());
            return Ok(Attempt::Done(Settled::Rejected(summary)));
        }
        Ok(Attempt::Pending(
            "the candidate revision is not active yet".into(),
        ))
    })
}
