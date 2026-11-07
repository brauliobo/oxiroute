#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpRelayPlan {
    policy: RtmpOutboundPolicy,
    max_queue_messages: usize,
    max_queue_bytes: usize,
    buffer_duration: Duration,
    push_reconnect_interval: Duration,
    pull_reconnect_interval: Duration,
    dns_refresh_interval: Duration,
    connect_timeout: Duration,
    handshake_timeout: Duration,
    push: Vec<RtmpPushPlan>,
    pull: Vec<RtmpPullPlan>,
}

impl RtmpRelayPlan {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        policy: RtmpOutboundPolicy,
        max_queue_messages: usize,
        max_queue_bytes: usize,
        buffer_duration: Duration,
        push_reconnect_interval: Duration,
        pull_reconnect_interval: Duration,
        dns_refresh_interval: Duration,
        connect_timeout: Duration,
        handshake_timeout: Duration,
        push: impl IntoIterator<Item = RtmpPushPlan>,
        pull: impl IntoIterator<Item = RtmpPullPlan>,
    ) -> Result<Self, RtmpPrepareError> {
        policy
            .validate_cidrs_intrinsic()
            .map_err(|source| value("relay.policy.cidrs").source(source))?;
        if max_queue_messages == 0
            || max_queue_bytes == 0
            || buffer_duration.is_zero()
            || push_reconnect_interval.is_zero()
            || pull_reconnect_interval.is_zero()
            || dns_refresh_interval.is_zero()
            || connect_timeout.is_zero()
            || handshake_timeout.is_zero()
        {
            return Err(bound("relay.bounds"));
        }
        Ok(Self {
            policy,
            max_queue_messages,
            max_queue_bytes,
            buffer_duration,
            push_reconnect_interval,
            pull_reconnect_interval,
            dns_refresh_interval,
            connect_timeout,
            handshake_timeout,
            push: push.into_iter().collect(),
            pull: pull.into_iter().collect(),
        })
    }
    #[must_use]
    pub const fn policy(&self) -> &RtmpOutboundPolicy {
        &self.policy
    }
    #[must_use]
    pub fn push(&self) -> &[RtmpPushPlan] {
        &self.push
    }
    #[must_use]
    pub fn pull(&self) -> &[RtmpPullPlan] {
        &self.pull
    }
    #[must_use]
    pub const fn max_queue_messages(&self) -> usize {
        self.max_queue_messages
    }
    #[must_use]
    pub const fn max_queue_bytes(&self) -> usize {
        self.max_queue_bytes
    }
    #[must_use]
    pub const fn buffer_duration(&self) -> Duration {
        self.buffer_duration
    }
    #[must_use]
    pub const fn push_reconnect_interval(&self) -> Duration {
        self.push_reconnect_interval
    }
    #[must_use]
    pub const fn pull_reconnect_interval(&self) -> Duration {
        self.pull_reconnect_interval
    }
    #[must_use]
    pub const fn dns_refresh_interval(&self) -> Duration {
        self.dns_refresh_interval
    }
    #[must_use]
    pub const fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }
    #[must_use]
    pub const fn handshake_timeout(&self) -> Duration {
        self.handshake_timeout
    }

    /// Resolves and assembles one push target without connecting to it.
    ///
    /// # Errors
    ///
    /// Returns a destination error when startup resolution, policy, family selection, or direct
    /// listener-loop validation fails.
    pub(crate) fn acquire_push_target<E>(
        &self,
        target: &RtmpPushPlan,
        listener_addresses: impl IntoIterator<Item = SocketAddr>,
        acquire_options: impl FnOnce() -> Result<crate::RtmpClientOptions, E>,
        map_destination_error: impl FnOnce(crate::RtmpDestinationResolverError) -> E,
    ) -> Result<RtmpPushTarget, E> {
        let resolver = self
            .acquire_resolver(
                target.host(),
                target.port(),
                target.transport(),
                listener_addresses,
            )
            .map_err(map_destination_error)?;
        let options = acquire_options()?;
        Ok(RtmpPushTarget {
            address: resolver.address(),
            host: target.host().to_owned(),
            transport: target.transport(),
            application: target.application().clone(),
            stream_name: target.stream_name().map(str::to_owned),
            options,
            config: self.runtime_config(self.push_reconnect_interval, resolver),
        })
    }

    /// Resolves and assembles one pull target without connecting to it.
    ///
    /// # Errors
    ///
    /// Returns a destination error when startup resolution, policy, family selection, or direct
    /// listener-loop validation fails.
    pub(crate) fn acquire_pull_target<E>(
        &self,
        target: &RtmpPullPlan,
        listener_addresses: impl IntoIterator<Item = SocketAddr>,
        acquire_options: impl FnOnce() -> Result<crate::RtmpClientOptions, E>,
        map_destination_error: impl FnOnce(crate::RtmpDestinationResolverError) -> E,
    ) -> Result<RtmpPullTarget, E> {
        let resolver = self
            .acquire_resolver(
                target.host(),
                target.port(),
                target.transport(),
                listener_addresses,
            )
            .map_err(map_destination_error)?;
        let options = acquire_options()?;
        Ok(RtmpPullTarget {
            address: resolver.address(),
            host: target.host().to_owned(),
            transport: target.transport(),
            source_application: target.source_application().to_owned(),
            source_stream_name: target.source_stream_name().to_owned(),
            local_application: target.local_application().to_owned(),
            local_stream_name: target.local_stream_name().to_owned(),
            options,
            config: self.runtime_config(self.pull_reconnect_interval, resolver),
        })
    }

    fn acquire_resolver(
        &self,
        host: &str,
        port: u16,
        transport: RtmpTransport,
        listener_addresses: impl IntoIterator<Item = SocketAddr>,
    ) -> Result<crate::RtmpDestinationResolver, crate::RtmpDestinationResolverError> {
        let addresses = crate::RtmpDestinationResolver::resolve_startup_addresses(host, port)
            .map_err(|_| crate::RtmpDestinationResolverError::EmptyAnswer)?;
        crate::RtmpDestinationResolver::from_startup(
            host.to_owned(),
            port,
            transport,
            addresses,
            self.policy.clone(),
            listener_addresses,
            self.dns_refresh_interval,
        )
    }

    fn runtime_config(
        &self,
        reconnect_interval: Duration,
        resolver: crate::RtmpDestinationResolver,
    ) -> crate::RtmpRelayConfig {
        crate::RtmpRelayConfig {
            max_queue_messages: self.max_queue_messages,
            max_queue_bytes: self.max_queue_bytes,
            buffer_duration: self.buffer_duration,
            connect_timeout: self.connect_timeout,
            handshake_timeout: self.handshake_timeout,
            reconnect_interval,
            max_chain_depth: self.policy.max_chain_depth,
            dns_resolver: Some(Arc::new(resolver)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpPushPlan {
    host: String,
    port: u16,
    transport: RtmpTransport,
    application: RtmpPushApplication,
    stream_name: Option<String>,
    client: RtmpClientPlan,
}

impl RtmpPushPlan {
    pub fn new(
        host: impl Into<String>,
        port: u16,
        transport: RtmpTransport,
        application: RtmpPushApplication,
        stream_name: Option<String>,
        client: RtmpClientPlan,
    ) -> Result<Self, RtmpPrepareError> {
        let host = host.into();
        validate_destination(&host, port)?;
        if let RtmpPushApplication::Exact(application) = &application {
            validate_application(application, "relay.push.application")?;
        }
        if let Some(stream_name) = &stream_name {
            validate_stream_name(stream_name, "relay.push.stream_name")?;
        }
        Ok(Self {
            host,
            port,
            transport,
            application,
            stream_name,
            client,
        })
    }
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }
    #[must_use]
    pub const fn client(&self) -> &RtmpClientPlan {
        &self.client
    }
    #[must_use]
    pub const fn transport(&self) -> RtmpTransport {
        self.transport
    }
    #[must_use]
    pub const fn application(&self) -> &RtmpPushApplication {
        &self.application
    }
    #[must_use]
    pub fn stream_name(&self) -> Option<&str> {
        self.stream_name.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpPullPlan {
    host: String,
    port: u16,
    transport: RtmpTransport,
    source_application: String,
    source_stream_name: String,
    local_application: String,
    local_stream_name: String,
    client: RtmpClientPlan,
}

impl RtmpPullPlan {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        host: impl Into<String>,
        port: u16,
        transport: RtmpTransport,
        source_application: impl Into<String>,
        source_stream_name: impl Into<String>,
        local_application: impl Into<String>,
        local_stream_name: impl Into<String>,
        client: RtmpClientPlan,
    ) -> Result<Self, RtmpPrepareError> {
        let host = host.into();
        validate_destination(&host, port)?;
        let source_application = source_application.into();
        let source_stream_name = source_stream_name.into();
        let local_application = local_application.into();
        let local_stream_name = local_stream_name.into();
        validate_application(&source_application, "relay.pull.source_application")?;
        validate_stream_name(&source_stream_name, "relay.pull.source_stream_name")?;
        validate_application(&local_application, "relay.pull.local_application")?;
        validate_stream_name(&local_stream_name, "relay.pull.local_stream_name")?;
        Ok(Self {
            host,
            port,
            transport,
            source_application,
            source_stream_name,
            local_application,
            local_stream_name,
            client,
        })
    }
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }
    #[must_use]
    pub const fn client(&self) -> &RtmpClientPlan {
        &self.client
    }
    #[must_use]
    pub const fn transport(&self) -> RtmpTransport {
        self.transport
    }
    #[must_use]
    pub fn source_application(&self) -> &str {
        &self.source_application
    }
    #[must_use]
    pub fn source_stream_name(&self) -> &str {
        &self.source_stream_name
    }
    #[must_use]
    pub fn local_application(&self) -> &str {
        &self.local_application
    }
    #[must_use]
    pub fn local_stream_name(&self) -> &str {
        &self.local_stream_name
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpClientPlan {
    flash_version: String,
    playback_buffer_ms: u32,
    tc_url: Option<String>,
    credential: Option<RtmpCredentialPlan>,
}

impl RtmpClientPlan {
    pub fn new(
        flash_version: impl Into<String>,
        playback_buffer_ms: u32,
        tc_url: Option<String>,
        credential: Option<RtmpCredentialPlan>,
    ) -> Result<Self, RtmpPrepareError> {
        let flash_version = flash_version.into();
        if flash_version.is_empty()
            || flash_version.len() > 128
            || flash_version.chars().any(char::is_control)
        {
            return Err(value("relay.client.flash_version"));
        }
        if tc_url.as_ref().is_some_and(|url| {
            url.len() > 2_048
                || (!url.starts_with("rtmp://") && !url.starts_with("rtmps://"))
                || url.contains([' ', '\n', '\r', '#'])
        }) {
            return Err(value("relay.client.tc_url"));
        }
        Ok(Self {
            flash_version,
            playback_buffer_ms,
            tc_url,
            credential,
        })
    }
    #[must_use]
    pub fn flash_version(&self) -> &str {
        &self.flash_version
    }
    #[must_use]
    pub const fn playback_buffer_ms(&self) -> u32 {
        self.playback_buffer_ms
    }
    #[must_use]
    pub fn tc_url(&self) -> Option<&str> {
        self.tc_url.as_deref()
    }
    #[must_use]
    pub const fn credential(&self) -> Option<&RtmpCredentialPlan> {
        self.credential.as_ref()
    }
}

impl fmt::Debug for RtmpClientPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtmpClientPlan")
            .field("flash_version", &self.flash_version)
            .field("playback_buffer_ms", &self.playback_buffer_ms)
            .field("tc_url", &self.tc_url.as_ref().map(|_| "<redacted>"))
            .field("credential", &self.credential)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpCredentialPlan {
    username: String,
    secret_file: PathBuf,
}
impl RtmpCredentialPlan {
    pub fn new(
        username: impl Into<String>,
        secret_file: PathBuf,
    ) -> Result<Self, RtmpPrepareError> {
        let username = username.into();
        if username.is_empty()
            || username.len() > 128
            || username.chars().any(char::is_control)
            || !valid_absolute_file_path(&secret_file)
        {
            return Err(value("relay.credential"));
        }
        Ok(Self {
            username,
            secret_file,
        })
    }
    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }
    #[must_use]
    pub fn secret_file(&self) -> &Path {
        &self.secret_file
    }
}
impl fmt::Debug for RtmpCredentialPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtmpCredentialPlan")
            .field("username", &self.username)
            .field("secret_file", &"<redacted>")
            .finish()
    }
}
