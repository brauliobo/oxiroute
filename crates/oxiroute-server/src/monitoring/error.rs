#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Debug)]
pub enum MetricsError {
    DuplicateListener(String),
    ListenerNotFound(String),
    InvalidListenerField(&'static str),
    InvalidListenerBind {
        listener: String,
        detail: &'static str,
    },
    ConnectionLimitReached {
        listener: String,
        limit: u64,
    },
    ProcessConnectionLimitReached {
        limit: u64,
    },
    AdministrativeDrain {
        resource: &'static str,
        name: String,
    },
    CounterOverflow(&'static str),
    StatePoisoned(&'static str),
    UnsupportedPlatform(&'static str),
    Io {
        path: &'static str,
        source: io::Error,
    },
    InvalidData {
        resource: &'static str,
        detail: String,
    },
    SystemClockBeforeUnixEpoch,
    ValueOutOfRange(&'static str),
}

impl fmt::Display for MetricsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateListener(name) => {
                write!(formatter, "listener `{name}` is already registered")
            }
            Self::ListenerNotFound(name) => {
                write!(formatter, "listener `{name}` is not registered")
            }
            Self::InvalidListenerField(field) => {
                write!(formatter, "listener {field} must not be empty")
            }
            Self::InvalidListenerBind { listener, detail } => {
                write!(
                    formatter,
                    "listener `{listener}` has an invalid bind: {detail}"
                )
            }
            Self::ConnectionLimitReached { listener, limit } => {
                write!(
                    formatter,
                    "listener `{listener}` reached its {limit}-connection limit"
                )
            }
            Self::ProcessConnectionLimitReached { limit } => {
                write!(formatter, "process reached its {limit}-connection limit")
            }
            Self::AdministrativeDrain { resource, name } => {
                write!(
                    formatter,
                    "{resource} `{name}` is not accepting new connections"
                )
            }
            Self::CounterOverflow(counter) => {
                write!(formatter, "metrics counter `{counter}` overflowed")
            }
            Self::StatePoisoned(state) => write!(formatter, "metrics state `{state}` is poisoned"),
            Self::UnsupportedPlatform(platform) => {
                write!(
                    formatter,
                    "process and host sampling is unsupported on {platform}"
                )
            }
            Self::Io { path, source } => write!(formatter, "failed to read `{path}`: {source}"),
            Self::InvalidData { resource, detail } => {
                write!(formatter, "invalid data in `{resource}`: {detail}")
            }
            Self::SystemClockBeforeUnixEpoch => {
                formatter.write_str("system clock predates the Unix epoch")
            }
            Self::ValueOutOfRange(value) => {
                write!(
                    formatter,
                    "sampled value `{value}` exceeds the supported range"
                )
            }
        }
    }
}

impl Error for MetricsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
