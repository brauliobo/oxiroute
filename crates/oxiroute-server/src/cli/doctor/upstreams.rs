use std::{
    io,
    net::{TcpStream, ToSocketAddrs as _},
    os::unix::net::UnixStream,
    thread,
    time::Duration,
};

use oxiroute_config::{ConfigDraft, UpstreamEndpoint, UpstreamPool};

use super::diagnosis::{Check, Diagnosis};
use crate::cli::context::Context;

fn reach(endpoint: &UpstreamEndpoint, limit: Duration) -> Result<(), String> {
    match endpoint {
        UpstreamEndpoint::Socket { address } => connect(address, limit),
        UpstreamEndpoint::Dns { host, port } => (host.as_str(), *port)
            .to_socket_addrs()
            .map_err(|error| format!("cannot resolve {host}: {error}"))?
            .map(|address| connect(&address, limit))
            .find_map(Result::err)
            .map_or(Ok(()), Err),
        UpstreamEndpoint::Unix { path } => UnixStream::connect(path)
            .map(drop)
            .map_err(|error| error.to_string()),
    }
}

fn connect(address: &std::net::SocketAddr, limit: Duration) -> Result<(), String> {
    TcpStream::connect_timeout(address, limit)
        .map(drop)
        .map_err(|error| match error.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
                "connection timed out".to_owned()
            }
            _ => error.to_string(),
        })
}

fn judge(pool: &UpstreamPool, failures: &[(String, String)]) -> Vec<Diagnosis> {
    let subject = format!("pool {}", pool.name);
    let fix = "start the server, or fix its address, firewall and DNS; `oxiroute server drain` takes it out of rotation";
    if failures.is_empty() {
        let message = format!("all {} servers accept connections", pool.servers.len());
        return vec![Diagnosis::ok(Check::Upstreams, subject, message)];
    }
    let described = failures
        .iter()
        .map(|(server, reason)| format!("{server}: {reason}"))
        .collect::<Vec<_>>();
    if failures.len() == pool.servers.len() {
        let message = format!("no server accepts connections ({})", described.join("; "));
        return vec![Diagnosis::critical(Check::Upstreams, subject, message).fixed_by(fix)];
    }
    described
        .into_iter()
        .map(|reason| Diagnosis::warning(Check::Upstreams, &subject, reason).fixed_by(fix))
        .collect()
}

pub(super) fn probe(draft: &ConfigDraft, context: &Context<'_>) -> Vec<Diagnosis> {
    match context.timeouts() {
        Ok(timeouts) => check(draft, timeouts.connect()),
        Err(_) => vec![Diagnosis::warning(
            Check::Upstreams,
            "upstreams",
            "not probed: the selected context cannot be resolved, see the contexts finding",
        )],
    }
}

pub(super) fn check(draft: &ConfigDraft, limit: Duration) -> Vec<Diagnosis> {
    thread::scope(|scope| {
        let probes = draft
            .upstream_pools
            .iter()
            .filter(|pool| !pool.servers.is_empty())
            .map(|pool| {
                let servers = pool.servers.iter().map(|server| {
                    scope.spawn(move || {
                        reach(&server.endpoint, limit).err().map(|reason| {
                            (format!("{} ({})", server.name, server.endpoint), reason)
                        })
                    })
                });
                (pool, servers.collect::<Vec<_>>())
            })
            .collect::<Vec<_>>();
        probes
            .into_iter()
            .flat_map(|(pool, handles)| {
                let failures = handles
                    .into_iter()
                    .filter_map(|handle| handle.join().ok()?)
                    .collect::<Vec<_>>();
                judge(pool, &failures)
            })
            .collect()
    })
}
