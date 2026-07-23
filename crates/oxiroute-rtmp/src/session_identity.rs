use crate::{RtmpStreamPath, RtmpStreamPathError, StreamKey};

pub(super) struct StreamIdentity {
    key: StreamKey,
}

impl StreamIdentity {
    pub(super) fn parse(
        service_id: &str,
        application: &str,
        protocol_name: &str,
    ) -> Result<Self, RtmpStreamPathError> {
        Ok(Self {
            key: RtmpStreamPath::parse(application, protocol_name)?.into_stream_key(service_id),
        })
    }

    pub(super) fn into_key(self) -> StreamKey {
        self.key
    }
}

pub(super) fn validate_application(application: &str) -> Result<(), RtmpStreamPathError> {
    RtmpStreamPath::validate_application(application)
}

pub(super) fn matches(key: &StreamKey, application: &str, protocol_name: &str) -> bool {
    RtmpStreamPath::matches_key(key, application, protocol_name)
}
