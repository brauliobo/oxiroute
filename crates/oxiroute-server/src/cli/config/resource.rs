use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use clap::{Args, Subcommand};
use oxiroute_config::ConfigDraft;
use oxiroute_config_source::{ConfigFormat, render_config, resolve_source_with_format};
use serde_json::{Map, Value, json};

use super::apply::{baseline, finish};
use crate::{
    cli::{context::Context, error::CliError, render, view::View},
    config::resource_patch::{
        ResourceCollection, ResourcePatch, apply_patch, get_collection_item, list_collection,
    },
};

const IF_CONFIG_REVISION: &str = "If-Config-Revision";

/// Declarative CRUD over configuration resources, supporting both the live daemon (persisting
/// back to its config file in any of the 7 formats) and offline files (`--file`).
#[derive(Subcommand)]
pub(crate) enum ResourceCommand {
    /// List all items in a configuration collection (`listeners`, `pools`, `http-services`,
    /// `certificates`, `tls-profiles`, `cache-stores`, `forward-proxies`, `rtmp-services`, `l4-services`).
    List {
        /// Collection name (e.g. `listeners`, `pools`, `http_services`, `certificates`).
        collection: String,
        /// Edit/inspect a local configuration file offline instead of talking to the daemon.
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
    },
    /// Show one named item from a configuration collection as JSON.
    Get {
        /// Collection name.
        collection: String,
        /// Item name.
        name: String,
        /// Read from a local configuration file offline instead of talking to the daemon.
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
    },
    /// Insert or replace a named item in a configuration collection.
    Set {
        /// Collection name.
        collection: String,
        /// JSON representation of the resource object.
        #[arg(
            long,
            value_name = "JSON",
            conflicts_with = "from_file",
            required_unless_present = "from_file"
        )]
        json: Option<String>,
        /// Path to a JSON file describing the resource object.
        #[arg(long, value_name = "FILE", conflicts_with = "json")]
        from_file: Option<PathBuf>,
        /// Overwrite an existing resource with the same name.
        #[arg(long)]
        replace: bool,
        #[command(flatten)]
        target: ResourceWriteTarget,
    },
    /// Remove a named item from a configuration collection.
    Remove {
        /// Collection name.
        collection: String,
        /// Name of the item to remove.
        name: String,
        #[command(flatten)]
        target: ResourceWriteTarget,
    },
    /// Set one or more root-level settings (`KEY=JSON_VALUE`, e.g. `listener_workers=4` or `log_filter="info"`).
    SetGlobal {
        /// Assignments as `KEY=VALUE` (JSON literal or bare string/number/bool/null).
        #[arg(required = true, num_args = 1..)]
        assignments: Vec<String>,
        #[command(flatten)]
        target: ResourceWriteTarget,
    },
    /// Manage static servers inside an upstream pool (`set` / `remove`).
    PoolServer {
        #[command(subcommand)]
        command: PoolServerCommand,
    },
    /// Manage routes inside an HTTP service (`add` / `remove`).
    HttpRoute {
        #[command(subcommand)]
        command: HttpRouteCommand,
    },
}

#[derive(Subcommand)]
pub(crate) enum PoolServerCommand {
    /// Add or replace a static server in an upstream pool.
    Set {
        /// Name of the upstream pool.
        pool: String,
        /// Full JSON server object (alternative to `--name` + `--address`).
        #[arg(long, value_name = "JSON", conflicts_with_all = ["name", "address"])]
        json: Option<String>,
        /// Server name (used with `--address`).
        #[arg(long, required_unless_present = "json")]
        name: Option<String>,
        /// Socket address `IP:PORT` (used with `--name`).
        #[arg(long, required_unless_present = "json")]
        address: Option<SocketAddr>,
        /// Optional static `max_connections` limit.
        #[arg(long)]
        max_connections: Option<u64>,
        /// Replace an existing server with the same name.
        #[arg(long)]
        replace: bool,
        #[command(flatten)]
        target: ResourceWriteTarget,
    },
    /// Remove a static server from an upstream pool.
    Remove {
        /// Name of the upstream pool.
        pool: String,
        /// Name of the server to remove.
        name: String,
        #[command(flatten)]
        target: ResourceWriteTarget,
    },
}

#[derive(Subcommand)]
pub(crate) enum HttpRouteCommand {
    /// Add (or replace at `--index`) a route on an HTTP service.
    Add {
        /// Name of the HTTP service.
        service: String,
        /// Full JSON route object (alternative to `--path` + `--pool`).
        #[arg(long, value_name = "JSON", conflicts_with_all = ["path", "pool", "host"])]
        json: Option<String>,
        /// Path prefix (e.g. `/api`).
        #[arg(long, required_unless_present = "json")]
        path: Option<String>,
        /// Target upstream pool name.
        #[arg(long, required_unless_present = "json")]
        pool: Option<String>,
        /// Optional exact host matcher.
        #[arg(long)]
        host: Option<String>,
        /// Replace the route at this zero-based index instead of appending.
        #[arg(long)]
        index: Option<usize>,
        #[command(flatten)]
        target: ResourceWriteTarget,
    },
    /// Remove a route by zero-based index from an HTTP service.
    Remove {
        /// Name of the HTTP service.
        service: String,
        /// Zero-based index of the route to remove.
        index: usize,
        #[command(flatten)]
        target: ResourceWriteTarget,
    },
}

#[derive(Args)]
pub(crate) struct ResourceWriteTarget {
    /// Patch a local configuration file offline instead of persisting through the daemon.
    #[arg(long, value_name = "FILE")]
    pub(super) file: Option<PathBuf>,
    /// Validate and preview the change without writing anything.
    #[arg(long)]
    pub(super) dry_run: bool,
    /// When talking to the daemon, wait until the new revision is active or rejected.
    #[arg(long, conflicts_with = "file")]
    pub(super) wait: bool,
}

pub(super) fn run_resource(
    context: &Context<'_>,
    command: &ResourceCommand,
) -> Result<(), CliError> {
    match command {
        ResourceCommand::List { collection, file } => {
            let col = parse_collection(collection)?;
            let draft = load_draft(context, file.as_deref())?;
            let items = list_collection(&draft, col).map_err(CliError::local)?;
            context.print(
                &json!({ "collection": col.json_key(), "items": items }),
                View::Generic,
            )
        }
        ResourceCommand::Get {
            collection,
            name,
            file,
        } => {
            let col = parse_collection(collection)?;
            let draft = load_draft(context, file.as_deref())?;
            let item = get_collection_item(&draft, col, name).map_err(CliError::local)?;
            context.print(&item, View::Generic)
        }
        ResourceCommand::Set {
            collection,
            json: raw_json,
            from_file,
            replace,
            target,
        } => {
            let col = parse_collection(collection)?;
            let item = read_json_input(raw_json.as_deref(), from_file.as_deref())?;
            execute_patch(
                context,
                target,
                ResourcePatch::Upsert {
                    collection: col,
                    item,
                    replace: *replace,
                },
            )
        }
        ResourceCommand::Remove {
            collection,
            name,
            target,
        } => {
            let col = parse_collection(collection)?;
            execute_patch(
                context,
                target,
                ResourcePatch::Remove {
                    collection: col,
                    name: name.clone(),
                },
            )
        }
        ResourceCommand::SetGlobal {
            assignments,
            target,
        } => {
            let mut fields = Map::new();
            for item in assignments {
                let Some((key, raw)) = item.split_once('=') else {
                    return Err(CliError::Local(format!(
                        "assignment `{item}` must be in KEY=VALUE form"
                    )));
                };
                let value = parse_cli_value(raw);
                fields.insert(key.trim().to_owned(), value);
            }
            execute_patch(context, target, ResourcePatch::SetGlobal { fields })
        }
        ResourceCommand::PoolServer { command } => run_pool_server(context, command),
        ResourceCommand::HttpRoute { command } => run_http_route(context, command),
    }
}

fn run_pool_server(context: &Context<'_>, command: &PoolServerCommand) -> Result<(), CliError> {
    match command {
        PoolServerCommand::Set {
            pool,
            json: raw_json,
            name,
            address,
            max_connections,
            replace,
            target,
        } => {
            let server = if let Some(raw) = raw_json {
                serde_json::from_str(raw).map_err(CliError::local)?
            } else {
                let mut obj = json!({
                    "name": name.as_deref().unwrap_or_default(),
                    "endpoint": {
                        "type": "socket",
                        "address": address.expect("clap requires address").to_string(),
                    }
                });
                if let Some(limit) = max_connections {
                    obj["max_connections"] = json!(limit);
                }
                obj
            };
            execute_patch(
                context,
                target,
                ResourcePatch::UpsertPoolServer {
                    pool: pool.clone(),
                    server,
                    replace: *replace,
                },
            )
        }
        PoolServerCommand::Remove { pool, name, target } => execute_patch(
            context,
            target,
            ResourcePatch::RemovePoolServer {
                pool: pool.clone(),
                name: name.clone(),
            },
        ),
    }
}

fn run_http_route(context: &Context<'_>, command: &HttpRouteCommand) -> Result<(), CliError> {
    match command {
        HttpRouteCommand::Add {
            service,
            json: raw_json,
            path,
            pool,
            host,
            index,
            target,
        } => {
            let route = if let Some(raw) = raw_json {
                serde_json::from_str(raw).map_err(CliError::local)?
            } else {
                let mut obj = json!({
                    "path": {
                        "kind": "segment_prefix",
                        "value": path.as_deref().unwrap_or("/"),
                    },
                    "action": {
                        "type": "proxy",
                        "upstream_pool": pool.as_deref().unwrap_or_default(),
                    }
                });
                if let Some(host_val) = host {
                    obj["host"] = json!({ "kind": "exact", "value": host_val });
                }
                obj
            };
            execute_patch(
                context,
                target,
                ResourcePatch::UpsertHttpRoute {
                    service: service.clone(),
                    route,
                    index: *index,
                },
            )
        }
        HttpRouteCommand::Remove {
            service,
            index,
            target,
        } => execute_patch(
            context,
            target,
            ResourcePatch::RemoveHttpRoute {
                service: service.clone(),
                index: *index,
            },
        ),
    }
}

fn parse_collection(name: &str) -> Result<ResourceCollection, CliError> {
    ResourceCollection::from_cli_name(name).ok_or_else(|| {
        CliError::Local(format!(
            "unknown collection `{name}`; expected one of: listeners, pools, http_services, certificates, tls_profiles, cache_stores, forward_proxy_services, rtmp_services, l4_services"
        ))
    })
}

fn read_json_input(raw: Option<&str>, file: Option<&Path>) -> Result<Value, CliError> {
    if let Some(raw) = raw {
        serde_json::from_str(raw).map_err(|err| CliError::Local(format!("invalid JSON: {err}")))
    } else if let Some(path) = file {
        let text = fs::read_to_string(path)
            .map_err(|err| CliError::Local(format!("cannot read {}: {err}", path.display())))?;
        serde_json::from_str(&text)
            .map_err(|err| CliError::Local(format!("invalid JSON in {}: {err}", path.display())))
    } else {
        Err(CliError::Local("pass --json or --from-file".into()))
    }
}

fn parse_cli_value(raw: &str) -> Value {
    let trimmed = raw.trim();
    serde_json::from_str(trimmed).unwrap_or_else(|_| Value::String(trimmed.to_owned()))
}

fn load_draft(context: &Context<'_>, file: Option<&Path>) -> Result<ConfigDraft, CliError> {
    if let Some(path) = file {
        let bytes = fs::read(path)
            .map_err(|err| CliError::Local(format!("cannot read {}: {err}", path.display())))?;
        let format = ConfigFormat::infer(path).map_err(CliError::local)?;
        let resolved = resolve_source_with_format(path, &bytes, format).map_err(CliError::local)?;
        return Ok(resolved.config.to_draft());
    }
    let body = context.client()?.get("/api/v1/config", true)?.json()?;
    let config_val = body
        .get("config")
        .cloned()
        .ok_or_else(|| CliError::Remote("daemon response missing `config`".into()))?;
    serde_json::from_value(config_val).map_err(CliError::local)
}

fn execute_patch(
    context: &Context<'_>,
    target: &ResourceWriteTarget,
    patch: ResourcePatch,
) -> Result<(), CliError> {
    if let Some(path) = &target.file {
        return execute_offline_patch(context, path, patch, target.dry_run);
    }
    let client = context.client()?;
    let snapshot = client.get("/api/v1/config", true)?.json()?;
    let disk_revision = snapshot
        .get("diskRevision")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Remote("daemon response missing `diskRevision`".into()))?
        .to_owned();
    let payload = json!({
        "patch": patch,
        "dryRun": target.dry_run,
    });
    if target.dry_run {
        let response = client.json("POST", "/api/v1/config/patch", &payload, &[])?;
        return context.print_response(&response, View::Generic);
    }
    let base = baseline(&client, target.wait)?;
    let saved = client
        .json(
            "POST",
            "/api/v1/config/patch",
            &payload,
            &[(IF_CONFIG_REVISION, disk_revision.as_str())],
        )?
        .json()?;
    finish(context, &client, &saved, base.as_ref())
}

fn execute_offline_patch(
    context: &Context<'_>,
    file_path: &Path,
    patch: ResourcePatch,
    dry_run: bool,
) -> Result<(), CliError> {
    let bytes = fs::read(file_path)
        .map_err(|err| CliError::Local(format!("cannot read {}: {err}", file_path.display())))?;
    let format = ConfigFormat::infer(file_path).map_err(CliError::local)?;
    let resolved =
        resolve_source_with_format(file_path, &bytes, format).map_err(CliError::local)?;
    if resolved.compositional {
        return Err(CliError::Local(format!(
            "{} is a compositional root; edit it via authored source (`config edit` / `config files edit`) instead of typed resource patching",
            file_path.display()
        )));
    }
    let updated_draft = apply_patch(&resolved.config.to_draft(), patch).map_err(CliError::local)?;
    let validated = updated_draft.validate().map_err(CliError::local)?;
    let rendered = render_config(format, &validated).map_err(CliError::local)?;
    if dry_run {
        return render::emit(&rendered);
    }
    fs::write(file_path, &rendered)
        .map_err(|err| CliError::Local(format!("cannot write {}: {err}", file_path.display())))?;
    if !context.quiet() {
        render::diagnose(&format!("updated {}", file_path.display()));
    }
    Ok(())
}
