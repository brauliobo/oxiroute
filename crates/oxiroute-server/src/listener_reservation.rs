use std::{collections::HashMap, io, path::PathBuf, sync::Arc};

use oxiroute_config::{Config, ListenerBind};

#[derive(Clone)]
pub struct ListenerReservation {
    inner: Arc<ReservationInner>,
}

struct ReservationInner {
    bind: ListenerBind,
    bind_text: String,
    #[cfg(unix)]
    socket: ReservedSocket,
    #[cfg(unix)]
    unix_socket: Option<UnixSocketIdentity>,
}

#[cfg(unix)]
enum ReservedSocket {
    Tcp(std::net::TcpListener),
    Unix(std::os::unix::net::UnixListener),
}

#[cfg(unix)]
struct UnixSocketIdentity {
    device: u64,
    inode: u64,
    path: PathBuf,
}

#[cfg(unix)]
impl UnixSocketIdentity {
    fn remove_if_unchanged(&self) {
        use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

        let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl Drop for ReservationInner {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(unix_socket) = &self.unix_socket {
            unix_socket.remove_if_unchanged();
        }
    }
}

impl ListenerReservation {
    /// Reserves a listener without starting accepts.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket cannot be bound securely or the transport is unsupported.
    pub fn bind(listener_name: &str, bind: &ListenerBind) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;

            let (socket, unix_socket, bind_text) = match bind {
                ListenerBind::Socket { address } => {
                    let listener = std::net::TcpListener::bind(address).map_err(|source| {
                        io::Error::new(
                            source.kind(),
                            format!(
                                "listener `{listener_name}` could not bind socket `{address}`: {source}"
                            ),
                        )
                    })?;
                    listener.set_nonblocking(true)?;
                    (ReservedSocket::Tcp(listener), None, address.to_string())
                }
                ListenerBind::Udp { address } => {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!(
                            "listener `{listener_name}` cannot reserve UDP socket `{address}` yet"
                        ),
                    ));
                }
                ListenerBind::Unix { path, mode } => {
                    let path_text = path.to_str().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!(
                                "listener `{listener_name}` Unix socket path is not valid UTF-8 and cannot be bound"
                            ),
                        )
                    })?;
                    let listener = std::os::unix::net::UnixListener::bind(path).map_err(|source| {
                        io::Error::new(
                            source.kind(),
                            format!(
                                "listener `{listener_name}` could not bind Unix socket `{path_text}`: {source}"
                            ),
                        )
                    })?;
                    listener.set_nonblocking(true)?;
                    if let Some(mode) = mode {
                        use std::os::unix::fs::PermissionsExt as _;
                        std::fs::set_permissions(
                            path,
                            std::fs::Permissions::from_mode(u32::from(*mode)),
                        )?;
                    }
                    let metadata = std::fs::symlink_metadata(path)?;
                    (
                        ReservedSocket::Unix(listener),
                        Some(UnixSocketIdentity {
                            device: metadata.dev(),
                            inode: metadata.ino(),
                            path: path.clone(),
                        }),
                        path_text.to_owned(),
                    )
                }
            };
            Ok(Self {
                inner: Arc::new(ReservationInner {
                    bind: bind.clone(),
                    bind_text,
                    socket,
                    unix_socket,
                }),
            })
        }
        #[cfg(not(unix))]
        {
            match bind {
                ListenerBind::Socket { address } => Ok(Self {
                    inner: Arc::new(ReservationInner {
                        bind: bind.clone(),
                        bind_text: address.to_string(),
                    }),
                }),
                ListenerBind::Udp { .. } | ListenerBind::Unix { .. } => Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("listener `{listener_name}` uses an unsupported transport"),
                )),
            }
        }
    }

    #[must_use]
    pub fn bind_config(&self) -> &ListenerBind {
        &self.inner.bind
    }

    #[must_use]
    pub fn bind_text(&self) -> &str {
        &self.inner.bind_text
    }

    /// Duplicates the reserved descriptor for one Pingora generation.
    ///
    /// # Errors
    ///
    /// Returns an error when the operating system cannot duplicate the descriptor.
    #[cfg(unix)]
    pub fn duplicate_fds(&self) -> io::Result<pingora::server::Fds> {
        use std::os::fd::IntoRawFd as _;

        let fd = match &self.inner.socket {
            ReservedSocket::Tcp(listener) => listener.try_clone()?.into_raw_fd(),
            ReservedSocket::Unix(listener) => listener.try_clone()?.into_raw_fd(),
        };
        let mut fds = pingora::server::Fds::new();
        fds.add(self.inner.bind_text.clone(), fd);
        Ok(fds)
    }
}

#[derive(Clone, Default)]
pub struct ListenerReservations {
    by_name: HashMap<String, ListenerReservation>,
}

impl ListenerReservations {
    /// Reserves every canonical and management listener, reusing matching process reservations.
    ///
    /// # Errors
    ///
    /// Returns an error without publishing the set when any new reservation fails.
    pub fn prepare(config: &Config, previous: Option<&Self>) -> io::Result<Self> {
        let mut by_name = HashMap::with_capacity(
            config.listeners.len()
                + usize::from(config.management.is_some())
                + config.stats.as_ref().map_or(0, |stats| stats.binds.len()),
        );
        for listener in &config.listeners {
            let reservation = previous
                .and_then(|reservations| reservations.by_bind(&listener.bind))
                .cloned()
                .map_or_else(
                    || ListenerReservation::bind(&listener.name, &listener.bind),
                    Ok,
                )?;
            by_name.insert(listener.name.clone(), reservation);
        }
        if let Some(management) = &config.management {
            let bind = ListenerBind::Socket {
                address: management.bind,
            };
            let reservation = previous
                .and_then(|reservations| reservations.by_bind(&bind))
                .cloned()
                .map_or_else(|| ListenerReservation::bind("management", &bind), Ok)?;
            by_name.insert("@management".into(), reservation);
        }
        if let Some(stats) = &config.stats {
            for (index, address) in stats.binds.iter().enumerate() {
                let name = format!("@stats-{index}");
                let bind = ListenerBind::Socket { address: *address };
                let reservation = previous
                    .and_then(|reservations| reservations.by_bind(&bind))
                    .cloned()
                    .map_or_else(|| ListenerReservation::bind(&name, &bind), Ok)?;
                by_name.insert(name, reservation);
            }
        }
        Ok(Self { by_name })
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ListenerReservation> {
        self.by_name.get(name)
    }

    fn by_bind(&self, bind: &ListenerBind) -> Option<&ListenerReservation> {
        self.by_name
            .values()
            .find(|reservation| same_bind_identity(reservation.bind_config(), bind))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

fn same_bind_identity(left: &ListenerBind, right: &ListenerBind) -> bool {
    match (left, right) {
        (ListenerBind::Socket { address: left }, ListenerBind::Socket { address: right })
        | (ListenerBind::Udp { address: left }, ListenerBind::Udp { address: right }) => {
            left == right
        }
        (ListenerBind::Unix { path: left, .. }, ListenerBind::Unix { path: right, .. }) => {
            left == right
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    use oxiroute_config::Config;

    use super::*;

    fn config(name: &str, address: SocketAddr) -> Config {
        Config {
            listeners: vec![oxiroute_config::Listener {
                name: name.into(),
                bind: ListenerBind::Socket { address },
                protocol: oxiroute_config::Protocol::Rtmp,
                service: Some("rtmp".into()),
                tls_profile: None,
                max_connections: None,
                downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            }],
            ..empty_config()
        }
    }

    fn empty_config() -> Config {
        Config {
            version: 1,
            max_connections: None,
            management: None,
            stats: None,
            certificates: Vec::new(),
            tls_profiles: Vec::new(),
            listeners: Vec::new(),
            cache_stores: Vec::new(),
            upstream_pools: Vec::new(),
            http_services: Vec::new(),
            forward_proxy_services: Vec::new(),
            rtmp_services: Vec::new(),
            l4_services: Vec::new(),
        }
    }

    #[test]
    fn matching_listener_reservations_are_reused_without_rebinding() {
        let address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("temporary bind")
            .local_addr()
            .expect("address");
        let first = ListenerReservations::prepare(&config("edge", address), None)
            .expect("first reservation");
        let second = ListenerReservations::prepare(&config("edge", address), Some(&first))
            .expect("reused reservation");

        assert!(Arc::ptr_eq(
            &first.get("edge").expect("first").inner,
            &second.get("edge").expect("second").inner
        ));
    }

    #[test]
    fn listener_reservations_are_reused_after_rename() {
        let address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("temporary bind")
            .local_addr()
            .expect("address");
        let first = ListenerReservations::prepare(&config("old-name", address), None)
            .expect("first reservation");
        let second = ListenerReservations::prepare(&config("new-name", address), Some(&first))
            .expect("renamed reservation");

        assert!(Arc::ptr_eq(
            &first.get("old-name").expect("first").inner,
            &second.get("new-name").expect("second").inner
        ));
    }

    #[test]
    fn statistics_reservations_are_reused_after_reorder() {
        let first_address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("first temporary bind")
            .local_addr()
            .expect("first address");
        let second_address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("second temporary bind")
            .local_addr()
            .expect("second address");
        let mut first_config = empty_config();
        first_config.stats = Some(oxiroute_config::Stats {
            binds: vec![first_address, second_address],
            admin_token_file: None,
        });
        let first = ListenerReservations::prepare(&first_config, None).expect("first reservations");
        let mut second_config = first_config;
        second_config.stats.as_mut().expect("stats").binds.reverse();
        let second = ListenerReservations::prepare(&second_config, Some(&first))
            .expect("reordered reservations");

        assert!(Arc::ptr_eq(
            &first.get("@stats-0").expect("first original").inner,
            &second.get("@stats-1").expect("first reordered").inner,
        ));
        assert!(Arc::ptr_eq(
            &first.get("@stats-1").expect("second original").inner,
            &second.get("@stats-0").expect("second reordered").inner,
        ));
    }
}
