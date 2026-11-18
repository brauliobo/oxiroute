use serde_json::json;

use super::ListenerCommand;
use crate::cli::{
    change::{Desired, Persist, Plan, Scope, Setting},
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
            Self::Ready { listeners, persist } => {
                administrative_state(context, listeners, "ready", *persist)
            }
            Self::Drain { listeners, persist } => {
                administrative_state(context, listeners, "drain", *persist)
            }
            Self::Maintenance { listeners, persist } => {
                administrative_state(context, listeners, "maintenance", *persist)
            }
        }
    }
}

fn administrative_state(
    context: &Context<'_>,
    listeners: &[String],
    state: &str,
    persist: Persist,
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
        &persist.body(json!({ "listeners": listeners, "state": state })),
        &plan,
    )
}
