use std::{
    io::{self, ErrorKind},
    net::{TcpListener, UdpSocket},
    os::unix::net::UnixStream,
    path::Path,
};

use oxiroute_config::{ConfigDraft, ListenerBind};

use super::diagnosis::{Check, Diagnosis};

const FIRST_UNPRIVILEGED_PORT: u16 = 1024;

pub(super) struct Host {
    pub(super) daemon_running: bool,
    pub(super) grants_privileged_ports: Option<bool>,
}

fn binds(draft: &ConfigDraft) -> Vec<(String, ListenerBind)> {
    let management = draft.management.iter().map(|management| {
        (
            "management".to_owned(),
            ListenerBind::Socket {
                address: management.bind,
                ipv6_only: false,
            },
        )
    });
    let listeners = draft
        .listeners
        .iter()
        .map(|listener| (format!("listener {}", listener.name), listener.bind.clone()));
    let stats = draft.stats.iter().flat_map(|stats| {
        stats.binds.iter().map(|address| {
            let bind = ListenerBind::Socket {
                address: *address,
                ipv6_only: false,
            };
            ("stats".to_owned(), bind)
        })
    });
    management.chain(listeners).chain(stats).collect()
}

fn probe(bind: &ListenerBind) -> io::Result<()> {
    match bind {
        ListenerBind::Socket { address, .. } => TcpListener::bind(address).map(drop),
        ListenerBind::Udp { address } => UdpSocket::bind(address).map(drop),
        ListenerBind::Unix { path, .. } => probe_socket(path),
    }
}

fn probe_socket(path: &Path) -> io::Result<()> {
    match UnixStream::connect(path) {
        Ok(_) => Err(ErrorKind::AddrInUse.into()),
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            ) =>
        {
            let parent = path.parent().unwrap_or(Path::new(""));
            if parent.as_os_str().is_empty() || parent.is_dir() {
                Ok(())
            } else {
                Err(ErrorKind::NotFound.into())
            }
        }
        Err(error) => Err(error),
    }
}

fn port(bind: &ListenerBind) -> Option<u16> {
    match bind {
        ListenerBind::Socket { address, .. } | ListenerBind::Udp { address } => {
            Some(address.port())
        }
        ListenerBind::Unix { .. } => None,
    }
}

fn busy_fix(bind: &ListenerBind) -> String {
    match bind {
        ListenerBind::Socket { address, .. } => {
            format!(
                "find the process with `ss -ltnp 'sport = :{}'`, stop it or change the bind",
                address.port()
            )
        }
        ListenerBind::Udp { address } => {
            format!(
                "find the process with `ss -lunp 'sport = :{}'`, stop it or change the bind",
                address.port()
            )
        }
        ListenerBind::Unix { path, .. } => {
            format!(
                "stop the process listening on {}, or change the bind",
                path.display()
            )
        }
    }
}

fn judge(name: &str, bind: &ListenerBind, error: &io::Error, host: &Host) -> Option<Diagnosis> {
    let subject = format!("{name} {bind}");
    let critical = |message: &str| Diagnosis::critical(Check::Listeners, &subject, message);
    match error.kind() {
        ErrorKind::AddrInUse if host.daemon_running => None,
        ErrorKind::AddrInUse => Some(
            critical("the address is already in use by another process").fixed_by(busy_fix(bind)),
        ),
        ErrorKind::AddrNotAvailable => Some(
            critical("the address is not assigned to this host")
                .fixed_by("bind to an address of this host, or to 0.0.0.0 or [::]"),
        ),
        ErrorKind::NotFound => Some(
            critical("the directory of the socket does not exist")
                .fixed_by("create it, or put the socket under the unit's RuntimeDirectory"),
        ),
        ErrorKind::PermissionDenied
            if port(bind).is_some_and(|port| port < FIRST_UNPRIVILEGED_PORT) =>
        {
            match host.grants_privileged_ports {
                Some(true) => None,
                Some(false) => Some(critical("binding a port below 1024 needs root or CAP_NET_BIND_SERVICE").fixed_by(
                    "add AmbientCapabilities=CAP_NET_BIND_SERVICE to the unit, or listen on a port above 1023",
                )),
                None => Some(
                    Diagnosis::warning(
                        Check::Listeners,
                        &subject,
                        "this user cannot bind a port below 1024; the daemon needs root or CAP_NET_BIND_SERVICE",
                    )
                    .fixed_by("run the daemon from the unit with AmbientCapabilities=CAP_NET_BIND_SERVICE, or listen on a port above 1023"),
                ),
            }
        }
        _ => Some(critical(&format!("the address cannot be bound: {error}"))),
    }
}

pub(super) fn check(draft: &ConfigDraft, host: &Host) -> Vec<Diagnosis> {
    let binds = binds(draft);
    let problems = binds
        .iter()
        .filter_map(|(name, bind)| judge(name, bind, &probe(bind).err()?, host))
        .collect::<Vec<_>>();
    let mut diagnoses = Vec::new();
    if problems.is_empty() && !binds.is_empty() {
        diagnoses.push(Diagnosis::ok(
            Check::Listeners,
            "binds",
            format!("the {} addresses of the configuration can be bound or are served by the running daemon", binds.len()),
        ));
    }
    diagnoses.extend(problems);
    diagnoses
}
