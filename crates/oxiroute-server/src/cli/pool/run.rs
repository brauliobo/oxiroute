use serde_json::json;

use super::PoolCommand;
use crate::cli::{
    change::{Desired, Persist, Plan, Scope, Setting},
    context::{Context, Execute},
    error::CliError,
    view::View,
};

impl Execute for PoolCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::List => context.show("/api/v1/pools", View::Pools),
            Self::Show { pool } => context.show_named("/api/v1/pools", "pools", pool, View::Pool),
            Self::Ready { pools, persist } => {
                administrative_state(context, pools, "ready", *persist)
            }
            Self::Drain { pools, persist } => {
                administrative_state(context, pools, "drain", *persist)
            }
            Self::Evacuate {
                pools,
                force,
                persist,
            } => context.mutate_as(
                "POST",
                "/api/v1/pools/evacuate",
                &persist.body(json!({ "pools": pools, "force": force })),
                View::Evacuation,
            ),
        }
    }
}

fn administrative_state(
    context: &Context<'_>,
    pools: &[String],
    state: &str,
    persist: Persist,
) -> Result<(), CliError> {
    let plan = Plan::new(
        Scope::Pool,
        Setting::State,
        Desired::Value(state.into()),
        pools.to_vec(),
    );
    context.change(
        "POST",
        "/api/v1/pools/administrative-state",
        &persist.body(json!({ "pools": pools, "state": state })),
        &plan,
    )
}
