use crate::{RtmpStreamPath, RtmpStreamPathError, StreamKey};

pub(super) struct StreamIdentity {
    key: StreamKey,
    query: Option<String>,
}

pub(super) struct VodIdentity {
    source: String,
    path: String,
    query: Option<String>,
}

impl StreamIdentity {
    pub(super) fn parse(
        service_id: &str,
        application: &str,
        protocol_name: &str,
    ) -> Result<Self, RtmpStreamPathError> {
        let path = RtmpStreamPath::parse(application, protocol_name)?;
        Ok(Self {
            key: path.stream_key(service_id),
            query: path.query().map(str::to_owned),
        })
    }

    pub(super) fn into_key(self) -> StreamKey {
        self.key
    }

    pub(super) fn stream_name(&self) -> &str {
        &self.key.name
    }

    pub(super) fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }
}

pub(super) fn validate_application(application: &str) -> Result<(), RtmpStreamPathError> {
    RtmpStreamPath::validate_application(application)
}

pub(super) fn matches(key: &StreamKey, application: &str, protocol_name: &str) -> bool {
    RtmpStreamPath::matches_key(key, application, protocol_name)
}

impl VodIdentity {
    pub(super) fn parse(protocol_name: &str) -> Option<Self> {
        let (name, query) = match protocol_name.split_once('?') {
            Some((name, query)) if !query.is_empty() => (name, Some(query.to_owned())),
            Some(_) => return None,
            None => (protocol_name, None),
        };
        let (source, path) = name.split_once('/')?;
        if !valid_component(source) || path.is_empty() || path.starts_with('/') {
            return None;
        }
        if path.len() > crate::MAX_VOD_PATH_BYTES
            || path.contains("//")
            || path.contains(['\\', '?', '#', '%'])
            || path.split('/').any(|component| {
                component.is_empty()
                    || component == "."
                    || component == ".."
                    || component.chars().any(char::is_control)
            })
        {
            return None;
        }
        Some(Self {
            source: source.to_owned(),
            path: path.to_owned(),
            query,
        })
    }

    pub(super) fn source(&self) -> &str {
        &self.source
    }

    pub(super) fn path(&self) -> &str {
        &self.path
    }

    pub(super) fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains(['/', '\\', '?', '#', '%'])
        && !value.chars().any(char::is_control)
}
