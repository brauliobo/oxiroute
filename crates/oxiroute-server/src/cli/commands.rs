#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn run(cli: &Cli) -> Result<(), CliError> {
    let endpoint = match &cli.socket {
        Some(path) => Endpoint::unix(&path.to_string_lossy())?,
        None => Endpoint::parse(&cli.endpoint)?,
    };
    let token = if command_requires_token(cli.command()) && !endpoint.is_unix() {
        let configured_token_file = std::env::var_os(MANAGEMENT_TOKEN_ENV);
        resolve_token_path(
            cli.token_file.as_deref(),
            configured_token_file.as_deref().map(Path::new),
            Path::new(DEFAULT_PACKAGE_ENV_FILE),
            Path::new(DEFAULT_MANAGEMENT_TOKEN_FILE),
        )?
        .map(|path| read_token(&path))
        .transpose()?
    } else {
        None
    };
    let client = Client { endpoint, token };
    if execute(cli, &client)? == FollowOutcome::Follow {
        follow_events(cli, &client)?;
    }
    Ok(())
}

#[derive(Eq, PartialEq)]
enum FollowOutcome {
    Complete,
    Follow,
}

#[allow(clippy::too_many_lines)]
fn execute(cli: &Cli, client: &Client) -> Result<FollowOutcome, CliError> {
    let (response, selection) = match cli.command() {
        Command::Status => (client.get("/api/v1/status", true)?, None),
        Command::Ready => (client.get("/ready", false)?, None),
        Command::Metrics => (client.get("/metrics", false)?, None),
        Command::Topology => (client.get("/api/v1/topology", true)?, None),
        Command::Monitoring => (client.get("/api/v1/monitoring", true)?, None),
        Command::Drain => (
            client.mutation_json("POST", "/api/v1/process/drain", &json!({}))?,
            None,
        ),
        Command::Shutdown => (
            client.mutation_json("POST", "/api/v1/process/shutdown", &json!({}))?,
            None,
        ),
        Command::Events {
            command: EventsCommand::List { after, limit },
        } => (
            client.get(&format!("/api/v1/events?after={after}&limit={limit}"), true)?,
            None,
        ),
        Command::Events {
            command: EventsCommand::Follow { .. },
        } => return Ok(FollowOutcome::Follow),
        Command::Server {
            command: ServerCommand::List { pool, origin },
        } => (
            registry::list(client, pool.as_deref())?,
            origin.map(Selection::Origin),
        ),
        Command::Server {
            command: ServerCommand::Register(args),
        } => {
            let registered = registry::register(client, args)?;
            if args.heartbeat {
                registry::keep_registered(cli, client, args, &registered)?;
                return Ok(FollowOutcome::Complete);
            }
            (registered, None)
        }
        Command::Server {
            command:
                ServerCommand::Deregister {
                    pool,
                    server,
                    missing_ok,
                },
        } => (
            registry::deregister(client, pool, server, *missing_ok)?,
            None,
        ),
        Command::Server {
            command: ServerCommand::Show(target),
        } => (
            client.get("/api/v1/servers", true)?,
            Some(Selection::Server(target)),
        ),
        Command::Server {
            command: ServerCommand::Ready(target),
        } => server_state(client, target, "ready")?,
        Command::Server {
            command: ServerCommand::Drain(target),
        } => server_state(client, target, "drain")?,
        Command::Server {
            command: ServerCommand::Maintenance(target),
        } => server_state(client, target, "maintenance")?,
        Command::Server {
            command: ServerCommand::SetHealth { target, health },
        } => (
            client.mutation_json(
                "POST",
                "/api/v1/servers/health-override",
                &server_body(target, Some(("health", health.as_str()))),
            )?,
            None,
        ),
        Command::Server {
            command: ServerCommand::Check { target, action },
        } => (
            client.mutation_json(
                "POST",
                "/api/v1/servers/checks",
                &server_bool_body(target, "enabled", matches!(action, Toggle::Enable)),
            )?,
            None,
        ),
        Command::Server {
            command:
                ServerCommand::MaxConnections {
                    command: MaxConnectionsCommand::Set { target, limit },
                },
        } => (
            client.mutation_json(
                "PUT",
                "/api/v1/servers/max-connections",
                &server_capacity_body(target, Some(*limit)),
            )?,
            None,
        ),
        Command::Server {
            command:
                ServerCommand::MaxConnections {
                    command: MaxConnectionsCommand::Reset(target),
                },
        } => (
            client.mutation_json(
                "PUT",
                "/api/v1/servers/max-connections",
                &server_capacity_body(target, None),
            )?,
            None,
        ),
        Command::Server {
            command:
                ServerCommand::Weight {
                    command: WeightCommand::Set { target, weight },
                },
        } => (
            client.mutation_json(
                "PUT",
                "/api/v1/servers/weight",
                &server_weight_body(target, Some(*weight)),
            )?,
            None,
        ),
        Command::Server {
            command:
                ServerCommand::Weight {
                    command: WeightCommand::Reset(target),
                },
        } => (
            client.mutation_json(
                "PUT",
                "/api/v1/servers/weight",
                &server_weight_body(target, None),
            )?,
            None,
        ),
        Command::Server {
            command: ServerCommand::RefreshDns(target),
        } => (
            client.mutation_json(
                "POST",
                "/api/v1/servers/refresh-dns",
                &server_body(target, None),
            )?,
            None,
        ),
        Command::Pool {
            command: PoolCommand::List,
        } => (client.get("/api/v1/pools", true)?, None),
        Command::Pool {
            command: PoolCommand::Show { pool },
        } => (
            client.get("/api/v1/pools", true)?,
            Some(Selection::Pool(pool)),
        ),
        Command::Pool {
            command: PoolCommand::Ready { pools },
        } => pool_state(client, pools, "ready")?,
        Command::Pool {
            command: PoolCommand::Drain { pools },
        } => pool_state(client, pools, "drain")?,
        Command::Listener {
            command: ListenerCommand::List,
        } => (client.get("/api/v1/listeners", true)?, None),
        Command::Listener {
            command: ListenerCommand::Show { listener },
        } => (
            client.get("/api/v1/listeners", true)?,
            Some(Selection::Listener(listener)),
        ),
        Command::Listener {
            command: ListenerCommand::Ready { listeners },
        } => listener_state(client, listeners, "ready")?,
        Command::Listener {
            command: ListenerCommand::Drain { listeners },
        } => listener_state(client, listeners, "drain")?,
        Command::Listener {
            command: ListenerCommand::Maintenance { listeners },
        } => listener_state(client, listeners, "maintenance")?,
        Command::Generation {
            command: GenerationCommand::Status,
        } => (client.get("/api/v1/generations", true)?, None),
        Command::Generation {
            command: GenerationCommand::Reload,
        } => (
            client.mutation_json("POST", "/api/v1/generations/reload", &json!({}))?,
            None,
        ),
        Command::Generation {
            command: GenerationCommand::Rollback,
        } => (
            client.mutation_json("POST", "/api/v1/generations/rollback", &json!({}))?,
            None,
        ),
        Command::Generation {
            command: GenerationCommand::Drain { timeout_ms },
        } => (
            client.mutation_json(
                "POST",
                "/api/v1/generations/drain",
                &json!({ "timeoutMs": timeout_ms }),
            )?,
            None,
        ),
        Command::Config {
            command: ConfigCommand::Get,
        } => (client.get("/api/v1/config", true)?, None),
        Command::Config {
            command: ConfigCommand::Validate { file },
        } => (
            client.json(
                "POST",
                "/api/v1/config/validate",
                &json!({ "config": config_file(file)? }),
                &[],
            )?,
            None,
        ),
        Command::Config {
            command: ConfigCommand::Apply { file },
        } => {
            let current = client.get("/api/v1/config", true)?.json()?;
            let revision = current
                .get("diskRevision")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    CliError::Local("configuration response has no disk revision".into())
                })?;
            (
                client.json(
                    "PUT",
                    "/api/v1/config",
                    &json!({ "config": config_file(file)? }),
                    &[("If-Config-Revision", revision)],
                )?,
                None,
            )
        }
        Command::Config {
            command: ConfigCommand::Diff { file },
        } => {
            let current = client.get("/api/v1/config", true)?.json()?;
            let active = current.get("config").cloned().ok_or_else(|| {
                CliError::Local("configuration response has no config field".into())
            })?;
            let candidate = config_file(file)?;
            let value = json!({ "different": active != candidate, "active": active, "candidate": candidate });
            print_value(&value, cli.output, cli.quiet)?;
            return Ok(FollowOutcome::Complete);
        }
        Command::Tls {
            command: TlsCommand::List,
        } => (client.get("/api/v1/tls", true)?, None),
        Command::Tls {
            command: TlsCommand::Reconcile { certificate },
        } => (
            client.mutation_json(
                "POST",
                "/api/v1/tls/reconcile",
                &json!({ "certificate": certificate }),
            )?,
            None,
        ),
        Command::Rtmp {
            command:
                RtmpCommand::Stream {
                    command: StreamCommand::List,
                },
        } => (client.get("/api/v1/rtmp/streams", true)?, None),
        Command::Rtmp {
            command:
                RtmpCommand::Stream {
                    command: StreamCommand::Show { stream },
                },
        } => (
            client.get(&format!("/api/v1/rtmp/streams/{stream}"), true)?,
            None,
        ),
        Command::Rtmp {
            command:
                RtmpCommand::Recorder {
                    command: RecorderCommand::Start { stream, recorder },
                },
        } => (
            client.generation_json(
                "POST",
                &format!("/api/v1/rtmp/streams/{stream}/recorders/{recorder}/start"),
                &json!({}),
            )?,
            None,
        ),
        Command::Rtmp {
            command:
                RtmpCommand::Recorder {
                    command: RecorderCommand::Stop { stream, recorder },
                },
        } => (
            client.generation_json(
                "POST",
                &format!("/api/v1/rtmp/streams/{stream}/recorders/{recorder}/stop"),
                &json!({}),
            )?,
            None,
        ),
        Command::Rtmp {
            command:
                RtmpCommand::Stream {
                    command: StreamCommand::Disconnect { .. },
                }
                | RtmpCommand::Relay {
                    command: RelayCommand::Reconnect { .. },
                },
        } => {
            return Err(CliError::Unsupported(
                "the active RTMP registry does not own a safe cancellation handle for this operation",
            ));
        }
        Command::Serve { .. }
        | Command::Import { .. }
        | Command::Version
        | Command::Registry { .. }
        | Command::Config {
            command: ConfigCommand::Check { .. } | ConfigCommand::Compose { .. },
        } => {
            return Err(CliError::Local(
                "local command was dispatched through the management client".into(),
            ));
        }
    };
    let value = response.output_value()?;
    let value = match selection {
        Some(selection) => selection.apply(value)?,
        None => value,
    };
    print_value(&value, cli.output, cli.quiet)?;
    Ok(FollowOutcome::Complete)
}

fn follow_events(cli: &Cli, client: &Client) -> Result<(), CliError> {
    let Command::Events {
        command:
            EventsCommand::Follow {
                after,
                limit,
                interval_ms,
            },
    } = cli.command()
    else {
        return Ok(());
    };
    let mut cursor = *after;
    loop {
        let value = client
            .get(
                &format!("/api/v1/events?after={cursor}&limit={limit}"),
                true,
            )?
            .json()?;
        if let Some(events) = value.get("events").and_then(Value::as_array) {
            for event in events {
                print_value(event, cli.output, cli.quiet)?;
            }
        }
        cursor = value
            .get("cursor")
            .and_then(Value::as_u64)
            .unwrap_or(cursor);
        thread::sleep(Duration::from_millis(*interval_ms));
    }
}

fn server_state(
    client: &Client,
    target: &ServerTarget,
    state: &str,
) -> Result<(Response, Option<Selection<'static>>), CliError> {
    Ok((
        client.mutation_json(
            "POST",
            "/api/v1/servers/administrative-state",
            &server_body(target, Some(("state", state))),
        )?,
        None,
    ))
}

fn pool_state(
    client: &Client,
    pools: &[String],
    state: &str,
) -> Result<(Response, Option<Selection<'static>>), CliError> {
    Ok((
        client.mutation_json(
            "POST",
            "/api/v1/pools/administrative-state",
            &json!({ "pools": pools, "state": state }),
        )?,
        None,
    ))
}

fn listener_state(
    client: &Client,
    listeners: &[String],
    state: &str,
) -> Result<(Response, Option<Selection<'static>>), CliError> {
    Ok((
        client.mutation_json(
            "POST",
            "/api/v1/listeners/administrative-state",
            &json!({ "listeners": listeners, "state": state }),
        )?,
        None,
    ))
}

fn server_targets(target: &ServerTarget) -> Vec<Value> {
    target
        .pools
        .iter()
        .map(|pool| json!({ "pool": pool, "server": target.server }))
        .collect()
}

fn server_body(target: &ServerTarget, field: Option<(&str, &str)>) -> Value {
    let mut body = json!({ "targets": server_targets(target) });
    if let Some((name, value)) = field {
        body.as_object_mut()
            .expect("object")
            .insert(name.into(), json!(value));
    }
    body
}

fn server_bool_body(target: &ServerTarget, field: &str, value: bool) -> Value {
    let mut body = server_body(target, None);
    body.as_object_mut()
        .expect("object")
        .insert(field.into(), json!(value));
    body
}

fn server_capacity_body(target: &ServerTarget, limit: Option<u64>) -> Value {
    let mut body = server_body(target, None);
    body.as_object_mut()
        .expect("object")
        .insert("maxConnections".into(), json!(limit));
    body
}

fn server_weight_body(target: &ServerTarget, weight: Option<u16>) -> Value {
    let mut body = server_body(target, None);
    body.as_object_mut()
        .expect("object")
        .insert("weight".into(), json!(weight));
    body
}

impl HealthValue {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

enum Selection<'a> {
    Origin(OriginFilter),
    Server(&'a ServerTarget),
    Pool(&'a str),
    Listener(&'a str),
}

impl Selection<'_> {
    fn apply(self, mut value: Value) -> Result<Value, CliError> {
        match self {
            Self::Origin(origin) => {
                let wanted = if origin == OriginFilter::Static {
                    "static"
                } else {
                    "dynamic"
                };
                let servers = value
                    .get_mut("servers")
                    .and_then(Value::as_array_mut)
                    .ok_or_else(|| {
                        CliError::Local("server response has no servers array".into())
                    })?;
                servers.retain(|entry| {
                    entry
                        .pointer("/origin")
                        .or_else(|| entry.pointer("/server/origin"))
                        .and_then(Value::as_str)
                        == Some(wanted)
                });
            }
            Self::Server(target) => {
                let servers = value
                    .get_mut("servers")
                    .and_then(Value::as_array_mut)
                    .ok_or_else(|| {
                        CliError::Local("server response has no servers array".into())
                    })?;
                servers.retain(|entry| {
                    target
                        .pools
                        .iter()
                        .any(|pool| entry.get("pool").and_then(Value::as_str) == Some(pool))
                        && entry.pointer("/server/name").and_then(Value::as_str)
                            == Some(&target.server)
                });
                if servers.is_empty() {
                    return Err(CliError::Status(404, "server_not_found".into()));
                }
                if !target.pools.iter().all(|pool| {
                    servers
                        .iter()
                        .any(|entry| entry.get("pool").and_then(Value::as_str) == Some(pool))
                }) {
                    return Err(CliError::Status(404, "server_not_found".into()));
                }
            }
            Self::Pool(name) => filter_named(&mut value, "pools", name)?,
            Self::Listener(name) => filter_named(&mut value, "listeners", name)?,
        }
        Ok(value)
    }
}

fn filter_named(value: &mut Value, field: &str, name: &str) -> Result<(), CliError> {
    let entries = value
        .get_mut(field)
        .and_then(Value::as_array_mut)
        .ok_or_else(|| CliError::Local(format!("response has no {field} array")))?;
    entries.retain(|entry| entry.get("name").and_then(Value::as_str) == Some(name));
    if entries.is_empty() {
        return Err(CliError::Status(404, format!("{field}_not_found")));
    }
    Ok(())
}

fn config_file(path: &Path) -> Result<Value, CliError> {
    let bytes = read_regular_file(path, MAX_FILE_BYTES, false)
        .map_err(|_| CliError::Local("configuration file could not be read securely".into()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| CliError::Local("configuration file must contain JSON".into()))?;
    Ok(value.get("config").cloned().unwrap_or(value))
}
