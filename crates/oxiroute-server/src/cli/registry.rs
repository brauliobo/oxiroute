#[allow(clippy::wildcard_imports)]
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use signal_hook::consts::{SIGINT, SIGTERM};

const SLICE: Duration = Duration::from_millis(100);

pub(super) fn parse_ttl(value: &str) -> Result<Duration, String> {
    let (digits, unit) = value.split_at(
        value
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(value.len()),
    );
    let amount = digits
        .parse::<u64>()
        .map_err(|_| "ttl must look like 500ms, 15s or 2m".to_owned())?;
    match unit {
        "ms" => Ok(Duration::from_millis(amount)),
        "s" => Ok(Duration::from_secs(amount)),
        "m" => Ok(Duration::from_secs(amount.saturating_mul(60))),
        _ => Err("ttl must look like 500ms, 15s or 2m".into()),
    }
}

pub(super) fn parse_label(value: &str) -> Result<(String, String), String> {
    value
        .split_once('=')
        .filter(|(key, _)| !key.is_empty())
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .ok_or_else(|| "label must be KEY=VALUE".to_owned())
}

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
            &read_regular_file(path, MAX_FILE_BYTES, false)
                .map_err(|_| CliError::Local("metadata file could not be read securely".into()))?,
        )
        .map_err(|_| CliError::Local("metadata file must contain JSON".into()))?,
        None => Value::Null,
    };
    let labels = args
        .labels
        .iter()
        .cloned()
        .collect::<std::collections::BTreeMap<_, _>>();
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
    cli: &Cli,
    client: &Client,
    args: &RegisterArgs,
    registered: &Response,
) -> Result<(), CliError> {
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [SIGTERM, SIGINT] {
        signal_hook::flag::register(signal, Arc::clone(&stop))
            .map_err(|error| CliError::Local(error.to_string()))?;
    }
    print_value(&registered.output_value()?, cli.output, cli.quiet)?;
    let ttl_ms = registered.json()?["ttlMs"]
        .as_u64()
        .ok_or_else(|| CliError::Remote("registration response has no ttlMs".into()))?;
    let renew_every = Duration::from_millis(ttl_ms / 3);
    let path = server_path(&args.pool, &args.server)?;
    while !sleep_unless_stopped(renew_every, &stop) {
        match client.json("POST", &format!("{path}/heartbeat"), &json!({}), &[]) {
            Ok(_) => {}
            Err(CliError::Status(404, _)) => drop(register(client, args)?),
            Err(error) => eprintln!("oxiroute: heartbeat failed, retrying: {error}"),
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

/// Appends a scoped token to `file` and returns its secret, which is never stored.
pub(super) fn create_token(
    file: &Path,
    id: &str,
    pools: &[String],
    name_prefix: Option<&str>,
    read: bool,
) -> Result<String, CliError> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let scope = if read {
        "read".to_owned()
    } else {
        let prefix =
            name_prefix.map_or_else(String::new, |prefix| format!(" name_prefix={prefix}"));
        format!("register pools={}{prefix}", pools.join(","))
    };
    let local = |message: &str| CliError::Local(message.into());
    let (secret, line) = crate::registry::auth::mint_token(id, &scope)
        .map_err(|_| local("a random secret could not be generated"))?;
    let existing = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(_) => return Err(local("tokens file could not be read")),
    };
    let candidate = format!(
        "{existing}{}{line}\n",
        if existing.is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        }
    );
    crate::registry::auth::TokenFile::parse(&candidate)
        .map_err(|_| local("the token id or scope is invalid or already used"))?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o640)
        .open(file)
        .and_then(|mut file| file.write_all(format!("{line}\n").as_bytes()))
        .map_err(|_| local("tokens file could not be written"))?;
    Ok(secret)
}
