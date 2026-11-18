use serde_json::json;

use super::ListenerCommand;
use crate::cli::{
    change::{Desired, Plan, Scope, Setting},
    context::{Context, Execute},
    error::CliError,
    view::View,
};

impl Execute for ListenerCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::List => context.show("/api/v1/listeners", View::Listeners),
            Self::Show { listener } => {
                context.show_named("/api/v1/listeners", "listeners", listener, View::Listener)
            }
            Self::Ready { listeners } => administrative_state(context, listeners, "ready"),
            Self::Drain { listeners } => administrative_state(context, listeners, "drain"),
            Self::Maintenance { listeners } => {
                administrative_state(context, listeners, "maintenance")
            }
        }
    }
}

fn administrative_state(
    context: &Context<'_>,
    listeners: &[String],
    state: &str,
) -> Result<(), CliError> {
    let plan = Plan::new(
        Scope::Listener,
        Setting::State,
        Desired::Value(state.into()),
        listeners.to_vec(),
    );
    context.change(
        "POST",
        "/api/v1/listeners/administrative-state",
        &json!({ "listeners": listeners, "state": state }),
        &plan,
    )
}
