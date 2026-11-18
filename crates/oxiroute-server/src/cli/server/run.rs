use serde_json::{Value, json};

use super::{
    args::{
        ChangeTarget, MaxConnectionsCommand, OriginFilter, RegisterArgs, ServerCommand,
        ServerTarget, Toggle, WeightCommand,
    },
    registration,
};
use crate::cli::{
    change::{Desired, Setting},
    context::{Context, Execute},
    error::CliError,
    view::View,
};

impl Execute for ServerCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        match self {
            Self::List { pool, origin } => list(context, pool.as_deref(), *origin),
            Self::Register(args) => register(context, args),
            Self::Deregister {
                pool,
                server,
                missing_ok,
            } => context.print_response(
                &registration::deregister(&context.client()?, pool, server, *missing_ok)?,
                View::Generic,
            ),
            Self::Show(target) => show(context, target),
            Self::Ready(target) => administrative_state(context, target, "ready"),
            Self::Drain(target) => administrative_state(context, target, "drain"),
            Self::Maintenance(target) => administrative_state(context, target, "maintenance"),
            Self::Evacuate { target, force } => context.mutate_as(
                "POST",
                "/api/v1/servers/evacuate",
                &target.body(json!({ "force": force })),
                View::Evacuation,
            ),
            Self::SetHealth { target, health } => context.change(
                "POST",
                "/api/v1/servers/health-override",
                &target.body(json!({ "health": health.as_str() })),
                &target.plan(Setting::Health, Desired::Value(health.as_str().into())),
            ),
            Self::Check { target, action } => context.change(
                "POST",
                "/api/v1/servers/checks",
                &target.body(json!({ "enabled": matches!(action, Toggle::Enable) })),
                &target.plan(Setting::Checks, Desired::Value(action.as_state().into())),
            ),
            Self::MaxConnections { command } => command.run(context),
            Self::Weight { command } => command.run(context),
            Self::RefreshDns(target) => context.mutate_as(
                "POST",
                "/api/v1/servers/refresh-dns",
                &target.body(json!({})),
                View::DnsRefresh,
            ),
        }
    }
}

impl Execute for MaxConnectionsCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        let (target, limit) = match self {
            Self::Set { target, limit } => (target, Some(*limit)),
            Self::Reset(target) => (target, None),
        };
        let desired = limit.map_or(Desired::Configured, |limit| {
            Desired::Value(limit.to_string())
        });
        context.change(
            "PUT",
            "/api/v1/servers/max-connections",
            &target.body(json!({ "maxConnections": limit })),
            &target.plan(Setting::MaxConnections, desired),
        )
    }
}

impl Execute for WeightCommand {
    fn run(&self, context: &Context<'_>) -> Result<(), CliError> {
        let (target, weight) = match self {
            Self::Set { target, weight } => (target, Some(*weight)),
            Self::Reset(target) => (target, None),
        };
        let desired = weight.map_or(Desired::Configured, |weight| {
            Desired::Value(weight.to_string())
        });
        context.change(
            "PUT",
            "/api/v1/servers/weight",
            &target.body(json!({ "weight": weight })),
            &target.plan(Setting::Weight, desired),
        )
    }
}

fn administrative_state(
    context: &Context<'_>,
    target: &ChangeTarget,
    state: &str,
) -> Result<(), CliError> {
    context.change(
        "POST",
        "/api/v1/servers/administrative-state",
        &target.body(json!({ "state": state })),
        &target.plan(Setting::State, Desired::Value(state.into())),
    )
}

fn list(
    context: &Context<'_>,
    pool: Option<&str>,
    origin: Option<OriginFilter>,
) -> Result<(), CliError> {
    let response = registration::list(&context.client()?, pool)?;
    let Some(origin) = origin else {
        return context.print_response(&response, View::Servers);
    };
    let mut value = response.output_value()?;
    servers_of(&mut value)?.retain(|entry| {
        entry
            .pointer("/origin")
            .or_else(|| entry.pointer("/server/origin"))
            .and_then(Value::as_str)
            == Some(origin.as_str())
    });
    context.print(&value, View::Servers)
}

fn show(context: &Context<'_>, target: &ServerTarget) -> Result<(), CliError> {
    let mut value = context
        .client()?
        .get("/api/v1/servers", true)?
        .output_value()?;
    let servers = servers_of(&mut value)?;
    servers.retain(|entry| {
        target
            .pools
            .iter()
            .any(|pool| entry.get("pool").and_then(Value::as_str) == Some(pool))
            && entry.pointer("/server/name").and_then(Value::as_str) == Some(&target.server)
    });
    let every_pool_matched = target.pools.iter().all(|pool| {
        servers
            .iter()
            .any(|entry| entry.get("pool").and_then(Value::as_str) == Some(pool))
    });
    if !every_pool_matched {
        return Err(CliError::Status(404, "server_not_found".into()));
    }
    context.print(&value, View::Server)
}

fn servers_of(value: &mut Value) -> Result<&mut Vec<Value>, CliError> {
    value
        .get_mut("servers")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| CliError::Local("server response has no servers array".into()))
}

fn register(context: &Context<'_>, args: &RegisterArgs) -> Result<(), CliError> {
    let client = context.client()?;
    let registered = registration::register(&client, args)?;
    if args.heartbeat {
        registration::keep_registered(context, &client, args, &registered)
    } else {
        context.print_response(&registered, View::Generic)
    }
}
