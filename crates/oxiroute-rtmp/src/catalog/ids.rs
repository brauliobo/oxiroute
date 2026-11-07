#[allow(clippy::wildcard_imports)]
use super::*;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(Uuid);

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

id_type!(StreamId);
id_type!(RecorderId);
id_type!(RelayId);
id_type!(OperationId);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtmpCapabilities {
    pub live_ingest: bool,
    pub manual_recording: bool,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StreamKey {
    pub server_id: String,
    pub application: String,
    pub name: String,
}

impl StreamKey {
    #[must_use]
    pub fn new(
        server_id: impl Into<String>,
        application: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            server_id: server_id.into(),
            application: application.into(),
            name: name.into(),
        }
    }
}

/// Parsed RTMP URL path with authentication/query data kept separate from stream identity.
#[derive(Clone, Eq, PartialEq)]
pub struct RtmpStreamPath {
    application: String,
    stream_name: String,
    query: Option<String>,
}

impl std::fmt::Debug for RtmpStreamPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RtmpStreamPath")
            .field("application", &self.application)
            .field("stream_name", &self.stream_name)
            .field("query", &self.query.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl RtmpStreamPath {
    /// Parses the application and stream components supplied by the RTMP protocol.
    ///
    /// Applications and stream names are single path components. A nonempty stream query is
    /// retained as protocol data but deliberately omitted from [`Self::stream_key`].
    ///
    /// # Errors
    ///
    /// Returns an error for empty, nested, fragmented, control-character, or ambiguous paths.
    pub fn parse(application: &str, protocol_name: &str) -> Result<Self, RtmpStreamPathError> {
        let (application, stream_name, query) = validated_rtmp_path(application, protocol_name)?;

        Ok(Self {
            application: application.to_owned(),
            stream_name: stream_name.to_owned(),
            query: query.map(str::to_owned),
        })
    }

    #[must_use]
    pub fn application(&self) -> &str {
        &self.application
    }

    #[must_use]
    pub fn stream_name(&self) -> &str {
        &self.stream_name
    }

    #[must_use]
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    #[must_use]
    pub fn stream_key(&self, service_id: impl Into<String>) -> StreamKey {
        StreamKey::new(service_id, &self.application, &self.stream_name)
    }

    #[must_use]
    pub fn into_stream_key(self, service_id: impl Into<String>) -> StreamKey {
        StreamKey::new(service_id, self.application, self.stream_name)
    }

    /// Validates an RTMP connection application before any application-owned clone is made.
    ///
    /// # Errors
    ///
    /// Returns a stable component or byte-limit error.
    pub fn validate_application(application: &str) -> Result<(), RtmpStreamPathError> {
        validate_application(application)
    }

    pub(crate) fn matches_key(key: &StreamKey, application: &str, protocol_name: &str) -> bool {
        validated_rtmp_path(application, protocol_name).is_ok_and(
            |(application, stream_name, _)| {
                key.application == application && key.name == stream_name
            },
        )
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error, Eq, PartialEq)]
pub enum RtmpStreamPathError {
    #[error("RTMP application must be one nonempty path component")]
    Application,
    #[error("RTMP stream name must be one nonempty path component")]
    StreamName,
    #[error("RTMP stream query must be nonempty and contain no fragment or control characters")]
    Query,
    #[error("RTMP application is {size} bytes; maximum is {maximum} bytes")]
    ApplicationTooLong { size: usize, maximum: usize },
    #[error("RTMP stream name is {size} bytes; maximum is {maximum} bytes")]
    StreamNameTooLong { size: usize, maximum: usize },
    #[error("RTMP stream query is {size} bytes; maximum is {maximum} bytes")]
    QueryTooLong { size: usize, maximum: usize },
}

fn validated_rtmp_path<'a>(
    application: &'a str,
    protocol_name: &'a str,
) -> Result<(&'a str, &'a str, Option<&'a str>), RtmpStreamPathError> {
    validate_application(application)?;
    let (stream_name, query) = match protocol_name.split_once('?') {
        Some((_, "")) => return Err(RtmpStreamPathError::Query),
        Some((stream_name, query)) => (stream_name, Some(query)),
        None => (protocol_name, None),
    };
    if stream_name.len() > MAX_RTMP_STREAM_NAME_BYTES {
        return Err(RtmpStreamPathError::StreamNameTooLong {
            size: stream_name.len(),
            maximum: MAX_RTMP_STREAM_NAME_BYTES,
        });
    }
    validate_path_component(stream_name).map_err(|()| RtmpStreamPathError::StreamName)?;
    if let Some(query) = query {
        if query.len() > MAX_RTMP_QUERY_BYTES {
            return Err(RtmpStreamPathError::QueryTooLong {
                size: query.len(),
                maximum: MAX_RTMP_QUERY_BYTES,
            });
        }
        if query.contains('#') || query.chars().any(char::is_control) {
            return Err(RtmpStreamPathError::Query);
        }
    }
    Ok((application, stream_name, query))
}

fn validate_application(application: &str) -> Result<(), RtmpStreamPathError> {
    if application.len() > MAX_RTMP_APPLICATION_BYTES {
        return Err(RtmpStreamPathError::ApplicationTooLong {
            size: application.len(),
            maximum: MAX_RTMP_APPLICATION_BYTES,
        });
    }
    validate_path_component(application).map_err(|()| RtmpStreamPathError::Application)?;
    if application.contains('?') {
        return Err(RtmpStreamPathError::Application);
    }
    Ok(())
}

fn validate_path_component(component: &str) -> Result<(), ()> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.contains(['/', '#'])
        || component.chars().any(char::is_control)
    {
        return Err(());
    }
    Ok(())
}
