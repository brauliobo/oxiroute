use serde_json::json;

use super::PoolCommand;
use crate::cli::{
    context::{Context, Execute},
    error::CliError,
};

impl Execute for PoolCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::List => context.show("/api/v1/pools"),
            Self::Show { pool } => context.show_named("/api/v1/pools", "pools", pool),
            Self::Ready { pools } => administrative_state(context, pools, "ready"),
            Self::Drain { pools } => administrative_state(context, pools, "drain"),
            Self::Evacuate { pools, force } => context.mutate(
                "POST",
                "/api/v1/pools/evacuate",
                &json!({ "pools": pools, "force": force }),
            ),
        }
    }
}

fn administrative_state(
    context: &Context<'_>,
    pools: &[String],
    state: &str,
) -> Result<(), CliError> {
    context.mutate(
        "POST",
        "/api/v1/pools/administrative-state",
        &json!({ "pools": pools, "state": state }),
    )
}
