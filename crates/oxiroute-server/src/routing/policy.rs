#[allow(clippy::wildcard_imports)]
use super::*;

/// Pool-level availability options compiled from the `resilience` configuration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct PoolResiliencePolicy {
    pub(super) fail_open: bool,
    /// Passive ejection leaves at least this many other servers selectable.
    pub(super) min_members: usize,
    /// How long a request waits for a server to become selectable before the pool is unavailable.
    pub(super) unavailable_wait: Option<Duration>,
}

impl PoolResiliencePolicy {
    pub(crate) fn from_config(resilience: &PoolResilience) -> Self {
        Self {
            fail_open: resilience.fail_open,
            min_members: resilience
                .min_members
                .map_or_else(|| usize::from(resilience.fail_open), usize::from),
            unavailable_wait: resilience
                .queue_while_unavailable_ms
                .map(Duration::from_millis),
        }
    }
}

#[derive(Clone)]
pub(crate) struct PoolConstructionBlueprint {
    pub(super) algorithm: UpstreamAlgorithm,
    pub(super) weights: Box<[u16]>,
    pub(super) readmission: Option<Arc<ReadmissionProbe>>,
    pub(super) queue: Option<Arc<QueueSchedule>>,
}

impl PoolConstructionBlueprint {
    pub(crate) fn compile(
        algorithm: &UpstreamAlgorithm,
        endpoint_count: usize,
        readmission: Option<&Readmission>,
        queue: Option<&PoolQueuePolicy>,
    ) -> Result<Self, PoolError> {
        if endpoint_count == 0 {
            return Err(PoolError::Empty);
        }
        let weights = effective_weights(algorithm, endpoint_count)?.into_boxed_slice();
        Ok(Self {
            algorithm: algorithm.clone(),
            weights,
            readmission: readmission
                .map(ReadmissionProbe::compile)
                .transpose()
                .map_err(|source| PoolError::InvalidReadmission(source.to_string()))?
                .map(Arc::new),
            queue: queue.map(|queue| Arc::new(QueueSchedule::from_config(queue))),
        })
    }
}

fn effective_weights(
    algorithm: &UpstreamAlgorithm,
    endpoint_count: usize,
) -> Result<Vec<u16>, PoolError> {
    match algorithm {
        UpstreamAlgorithm::WeightedRoundRobin { weights } => {
            if weights.len() != endpoint_count {
                return Err(PoolError::InvalidWeights {
                    detail: "weighted round-robin requires one weight per endpoint",
                });
            }
            if weights.contains(&0) {
                return Err(PoolError::InvalidWeights {
                    detail: "weighted round-robin weights must be positive",
                });
            }
            Ok(weights.clone())
        }
        UpstreamAlgorithm::RoundRobin
        | UpstreamAlgorithm::LeastConnections
        | UpstreamAlgorithm::PowerOfTwoChoices
        | UpstreamAlgorithm::First => Ok(vec![1; endpoint_count]),
    }
}

pub(super) const fn algorithm_name(algorithm: &UpstreamAlgorithm) -> &'static str {
    match algorithm {
        UpstreamAlgorithm::RoundRobin => "round_robin",
        UpstreamAlgorithm::WeightedRoundRobin { .. } => "weighted_round_robin",
        UpstreamAlgorithm::LeastConnections => "least_connections",
        UpstreamAlgorithm::PowerOfTwoChoices => "power_of_two_choices",
        UpstreamAlgorithm::First => "first",
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PoolAdminError {
    #[error("unknown upstream server `{0}`")]
    UnknownServer(String),
    #[error("max-connections must be greater than zero")]
    InvalidMaxConnections,
    #[error("weight must be between 1 and 100")]
    InvalidWeight,
    #[error("upstream DNS refresh failed")]
    DnsRefreshFailed,
}

/// Errors produced while constructing an endpoint pool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PoolError {
    Empty,
    InvalidWeights { detail: &'static str },
    InvalidPassivePolicy { detail: &'static str },
    InvalidReadmission(String),
    StartupDns { server: String, detail: String },
    InvalidSocketEndpoint(SocketAddr),
    InvalidDnsEndpoint { host: String, port: u16 },
    InvalidUnixEndpoint(PathBuf),
    ProtectedEndpoint { server: String },
}

impl fmt::Display for PoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("upstream endpoint pool cannot be empty"),
            Self::InvalidWeights { detail } => {
                write!(
                    formatter,
                    "invalid weighted round-robin configuration: {detail}"
                )
            }
            Self::InvalidPassivePolicy { detail } => {
                write!(formatter, "invalid passive failure policy: {detail}")
            }
            Self::InvalidReadmission(detail) => {
                write!(formatter, "invalid probe re-admission policy: {detail}")
            }
            Self::StartupDns { server, detail } => {
                write!(
                    formatter,
                    "upstream server `{server}` startup DNS resolution failed: {detail}"
                )
            }
            Self::InvalidSocketEndpoint(address) => {
                write!(formatter, "invalid socket endpoint `{address}`")
            }
            Self::InvalidDnsEndpoint { host, port } => {
                write!(formatter, "invalid DNS endpoint `{host}:{port}`")
            }
            Self::InvalidUnixEndpoint(path) => {
                write!(formatter, "invalid Unix endpoint `{}`", path.display())
            }
            Self::ProtectedEndpoint { server } => write!(
                formatter,
                "upstream server `{server}` resolves to a protected management or statistics listener"
            ),
        }
    }
}

impl Error for PoolError {}
