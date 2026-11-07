#[allow(clippy::wildcard_imports)]
use super::*;

/// Value-only preparation inputs. Candidate addresses are sorted and deduplicated; they are not
/// reserved, bound, or checked against the host network here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpPrepareContext {
    mode: RtmpPrepareMode,
    candidate_listener_addresses: Vec<SocketAddr>,
}

impl RtmpPrepareContext {
    #[must_use]
    pub fn new(
        mode: RtmpPrepareMode,
        candidate_listener_addresses: impl IntoIterator<Item = SocketAddr>,
    ) -> Self {
        let mut candidate_listener_addresses: Vec<_> =
            candidate_listener_addresses.into_iter().collect();
        candidate_listener_addresses.sort_unstable();
        candidate_listener_addresses.dedup();
        Self {
            mode,
            candidate_listener_addresses,
        }
    }

    #[must_use]
    pub const fn mode(&self) -> RtmpPrepareMode {
        self.mode
    }

    #[must_use]
    pub fn candidate_listener_addresses(&self) -> &[SocketAddr] {
        &self.candidate_listener_addresses
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RtmpPrepareCategory {
    Identity,
    Bound,
    Value,
}

#[derive(Debug)]
pub struct RtmpPrepareError {
    category: RtmpPrepareCategory,
    field: &'static str,
    context: Box<RtmpPrepareErrorContext>,
    source: Option<Box<RtmpPrepareSource>>,
}

impl fmt::Display for RtmpPrepareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid RTMP ")?;
        write!(formatter, "{:?} at {}", self.category, self.field)?;
        if let Some(service) = &self.context.service {
            write!(formatter, " in service `{service}`")?;
        }
        if let Some(application) = &self.context.application {
            write!(formatter, ", application `{application}`")?;
        }
        if let Some(recorder) = &self.context.recorder {
            write!(formatter, ", recorder `{recorder}`")?;
        }
        if let Some(profile) = &self.context.profile {
            write!(formatter, ", profile `{profile}`")?;
        }
        Ok(())
    }
}

impl std::error::Error for RtmpPrepareError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

#[derive(Debug, Default)]
struct RtmpPrepareErrorContext {
    service: Option<String>,
    application: Option<String>,
    recorder: Option<String>,
    profile: Option<String>,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RtmpPrepareSource {
    #[error(transparent)]
    OutboundPolicy(#[from] DestinationPolicyError),
    #[error(transparent)]
    Session(#[from] RtmpSessionLimitError),
    #[error(transparent)]
    Exec(#[from] ExecProfileError),
    #[error(transparent)]
    RecorderWorker(#[from] RecorderWorkerStartError),
    #[error(transparent)]
    RecordingStore(#[from] RecordingStoreLimitsError),
    #[error(transparent)]
    Hls(#[from] HlsValueError),
    #[error(transparent)]
    AutoPush(#[from] RtmpAutoPushConfigError),
    #[error(transparent)]
    Callback(#[from] RtmpCallbackValueError),
    #[error(transparent)]
    Vod(#[from] VodValueError),
}

impl RtmpPrepareError {
    pub(super) fn new(category: RtmpPrepareCategory, field: &'static str) -> Self {
        Self {
            category,
            field,
            context: Box::default(),
            source: None,
        }
    }

    #[must_use]
    pub fn with_service(mut self, value: impl Into<String>) -> Self {
        self.context.service = Some(value.into());
        self
    }
    #[must_use]
    pub fn with_application(mut self, value: impl Into<String>) -> Self {
        self.context.application = Some(value.into());
        self
    }
    #[must_use]
    pub fn with_recorder(mut self, value: impl Into<String>) -> Self {
        self.context.recorder = Some(value.into());
        self
    }
    #[must_use]
    pub fn with_profile(mut self, value: impl Into<String>) -> Self {
        self.context.profile = Some(value.into());
        self
    }
    #[must_use]
    pub fn contextualize_service(self, value: impl Into<String>) -> Self {
        self.with_service(value)
    }
    #[must_use]
    pub fn contextualize_application(self, value: impl Into<String>) -> Self {
        self.with_application(value)
    }
    #[must_use]
    pub fn contextualize_recorder(self, value: impl Into<String>) -> Self {
        self.with_recorder(value)
    }
    #[must_use]
    pub fn contextualize_profile(self, value: impl Into<String>) -> Self {
        self.with_profile(value)
    }
    pub(super) fn source(mut self, value: impl Into<RtmpPrepareSource>) -> Self {
        self.source = Some(Box::new(value.into()));
        self
    }

    #[must_use]
    pub const fn category(&self) -> RtmpPrepareCategory {
        self.category
    }
    #[must_use]
    pub const fn field(&self) -> &'static str {
        self.field
    }
    #[must_use]
    pub fn service_id(&self) -> Option<&str> {
        self.context.service.as_deref()
    }
    #[must_use]
    pub fn application_name(&self) -> Option<&str> {
        self.context.application.as_deref()
    }
    #[must_use]
    pub fn recorder_name(&self) -> Option<&str> {
        self.context.recorder.as_deref()
    }
    #[must_use]
    pub fn profile_name(&self) -> Option<&str> {
        self.context.profile.as_deref()
    }
    #[must_use]
    pub fn prepare_source(&self) -> Option<&RtmpPrepareSource> {
        self.source.as_deref()
    }
}
