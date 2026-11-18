use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use serde_json::{Value, json};
use signal_hook::consts::{SIGINT, SIGTERM};

use super::args::RegisterArgs;
use crate::cli::{
    context::Context,
    error::CliError,
    fs::{MAX_FILE_BYTES, read_regular_file},
    net::{Client, Response},
    render,
};

const SLICE: Duration = Duration::from_millis(100);

fn segment(value: &str) -> Result<&str, CliError> {
    let lexical = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
    if value.bytes().all(lexical) {
        Ok(value)
    } else {
        Err(CliError::Local(
            "pool and server names use letters, digits, `.`, `_` and `-`".into(),
        ))
    }
}

fn server_path(pool: &str, server: &str) -> Result<String, CliError> {
    Ok(format!(
        "/api/v1/pools/{}/servers/{}",
        segment(pool)?,
        segment(server)?
    ))
}

pub(super) fn list(client: &Client, pool: Option<&str>) -> Result<Response, CliError> {
    match pool {
        Some(pool) => client.get(&format!("/api/v1/pools/{}/servers", segment(pool)?), true),
        None => client.get("/api/v1/servers", true),
    }
}

fn registration_body(args: &RegisterArgs) -> Result<Value, CliError> {
    let metadata = match &args.metadata_file {
        Some(path) => serde_json::from_slice(
            &read_regular_file(path, MAX_FILE_BYTES, false).map_err(|error| {
                CliError::Local(format!(
                    "cannot read metadata file {}: {error}",
                    path.display()
                ))
            })?,
        )
        .map_err(|error| {
            CliError::Local(format!(
                "metadata file {} must contain JSON: {error}",
                path.display()
            ))
        })?,
        None => Value::Null,
    };
    let labels = args.labels.iter().cloned().collect::<BTreeMap<_, _>>();
    Ok(json!({
        "address": args.address.to_string(),
        "weight": args.weight,
        "priority": args.priority,
        "maxConnections": args.max_connections,
        "ttlMs": args.ttl.map(|ttl| u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX)),
        "labels": labels,
        "metadata": metadata,
    }))
}

pub(super) fn register(client: &Client, args: &RegisterArgs) -> Result<Response, CliError> {
    let path = server_path(&args.pool, &args.server)?;
    client.json("PUT", &path, &registration_body(args)?, &[])
}

pub(super) fn deregister(
    client: &Client,
    pool: &str,
    server: &str,
    missing_ok: bool,
) -> Result<Response, CliError> {
    let deregistered = match client.request("DELETE", &server_path(pool, server)?, &[], &[], true) {
        Ok(_) => true,
        Err(CliError::Status(404, _)) if missing_ok => false,
        Err(error) => return Err(error),
    };
    Ok(Response {
        status: 200,
        content_type: "application/json".into(),
        body: json!({ "pool": pool, "server": server, "deregistered": deregistered })
            .to_string()
            .into_bytes(),
    })
}

/// Keeps the registration alive until SIGTERM or SIGINT, then announces a drain and leaves.
pub(super) fn keep_registered(
    context: &Context<'_>,
    client: &Client,
    args: &RegisterArgs,
    registered: &Response,
) -> Result<(), CliError> {
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [SIGTERM, SIGINT] {
        signal_hook::flag::register(signal, Arc::clone(&stop))
            .map_err(|error| CliError::Local(error.to_string()))?;
    }
    context.print(&registered.output_value()?)?;
    let ttl_ms = registered.json()?["ttlMs"]
        .as_u64()
        .ok_or_else(|| CliError::Remote("registration response has no ttlMs".into()))?;
    let renew_every = Duration::from_millis(ttl_ms / 3);
    let path = server_path(&args.pool, &args.server)?;
    while !sleep_unless_stopped(renew_every, &stop) {
        match client.json("POST", &format!("{path}/heartbeat"), &json!({}), &[]) {
            Ok(_) => {}
            Err(CliError::Status(404, _)) => drop(register(client, args)?),
            Err(error) => {
                render::diagnose(&format!("oxiroute: heartbeat failed, retrying: {error}"));
            }
        }
    }
    client.json(
        "POST",
        &format!("{path}/heartbeat"),
        &json!({ "state": "draining" }),
        &[],
    )?;
    deregister(client, &args.pool, &args.server, true).map(drop)
}

/// Sleeps in short slices; true when a stop signal arrived.
fn sleep_unless_stopped(total: Duration, stop: &AtomicBool) -> bool {
    let mut waited = Duration::ZERO;
    while waited < total && !stop.load(Ordering::Relaxed) {
        thread::sleep(SLICE);
        waited += SLICE;
    }
    stop.load(Ordering::Relaxed)
}
