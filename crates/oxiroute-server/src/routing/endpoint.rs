#[allow(clippy::wildcard_imports)]
use super::*;

/// A validated upstream endpoint retained in its canonical, typed form at runtime.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum RuntimeEndpoint {
    Socket { address: SocketAddr },
    Dns { host: String, port: u16 },
    Unix { path: PathBuf },
}

pub(crate) const MAX_RESOLVED_ENDPOINT_ADDRESSES: usize = 16;

impl RuntimeEndpoint {
    pub(super) fn preflight(&self) -> Result<(), PoolError> {
        match self {
            Self::Socket { address } if address.port() == 0 => {
                Err(PoolError::InvalidSocketEndpoint(*address))
            }
            Self::Dns { host, port }
                if *port == 0 || host.parse::<IpAddr>().is_ok() || !is_dns_name(host) =>
            {
                Err(PoolError::InvalidDnsEndpoint {
                    host: host.clone(),
                    port: *port,
                })
            }
            Self::Unix { path } if !valid_unix_endpoint(path) => {
                Err(PoolError::InvalidUnixEndpoint(path.clone()))
            }
            Self::Socket { .. } | Self::Dns { .. } | Self::Unix { .. } => Ok(()),
        }
    }

    pub(crate) async fn resolve_addresses(&self) -> io::Result<Vec<SocketAddr>> {
        match self {
            Self::Socket { address } => Ok(vec![*address]),
            Self::Dns { host, port } => {
                let addresses = tokio::net::lookup_host((host.as_str(), *port)).await?;
                self.order_addresses(addresses)
            }
            Self::Unix { path } => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Unix endpoint `{}` does not resolve through DNS",
                    path.display()
                ),
            )),
        }
    }

    pub(crate) fn order_addresses(
        &self,
        addresses: impl IntoIterator<Item = SocketAddr>,
    ) -> io::Result<Vec<SocketAddr>> {
        let mut normalized = Vec::with_capacity(MAX_RESOLVED_ENDPOINT_ADDRESSES);
        for address in addresses {
            if normalized.contains(&address) {
                continue;
            }
            if normalized.len() == MAX_RESOLVED_ENDPOINT_ADDRESSES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "DNS endpoint `{self}` returned more than {MAX_RESOLVED_ENDPOINT_ADDRESSES} addresses"
                    ),
                ));
            }
            normalized.push(address);
        }
        let mut addresses = normalized;
        addresses.sort_unstable();
        if addresses.is_empty() {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("DNS endpoint `{self}` returned no addresses"),
            ))
        } else {
            Ok(addresses)
        }
    }
}

impl From<SocketAddr> for RuntimeEndpoint {
    fn from(address: SocketAddr) -> Self {
        Self::Socket { address }
    }
}

impl TryFrom<&UpstreamEndpoint> for RuntimeEndpoint {
    type Error = PoolError;

    fn try_from(endpoint: &UpstreamEndpoint) -> Result<Self, Self::Error> {
        let endpoint = match endpoint {
            UpstreamEndpoint::Socket { address } => Self::Socket { address: *address },
            UpstreamEndpoint::Dns { host, port } => Self::Dns {
                host: host.clone(),
                port: *port,
            },
            UpstreamEndpoint::Unix { path } => Self::Unix { path: path.clone() },
        };
        endpoint.preflight()?;
        Ok(endpoint)
    }
}

impl RuntimeEndpoint {
    pub(crate) fn compile(endpoint: &UpstreamEndpoint) -> Result<Self, PoolError> {
        Self::try_from(endpoint)
    }
}

impl fmt::Display for RuntimeEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Socket { address } => address.fmt(formatter),
            Self::Dns { host, port } => write!(formatter, "{host}:{port}"),
            Self::Unix { path } => path.display().fmt(formatter),
        }
    }
}

impl Serialize for RuntimeEndpoint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

fn valid_unix_endpoint(path: &Path) -> bool {
    if path.to_str().is_none() || !path.is_absolute() {
        return false;
    }
    #[cfg(unix)]
    {
        std::os::unix::net::SocketAddr::from_pathname(path).is_ok()
    }
    #[cfg(not(unix))]
    {
        false
    }
}
