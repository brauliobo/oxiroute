use crate::{RtmpStreamPath, RtmpStreamPathError, StreamKey};

pub(super) struct StreamIdentity {
    key: StreamKey,
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
