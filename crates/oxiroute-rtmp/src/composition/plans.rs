#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpAutoPushPlan {
    config: RtmpAutoPushConfig,
}
impl RtmpAutoPushPlan {
    pub fn new(config: RtmpAutoPushConfig) -> Result<Self, RtmpPrepareError> {
        config
            .validate_intrinsic()
            .map_err(|source| value("auto_push.config").source(source))?;
        Ok(Self { config })
    }
    #[must_use]
    pub const fn config(&self) -> &RtmpAutoPushConfig {
        &self.config
    }
}
impl fmt::Debug for RtmpAutoPushPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtmpAutoPushPlan")
            .field("enabled", &self.config.enabled)
            .field("socket_dir", &"<redacted>")
            .field(
                "secret_file",
                &self.config.secret_file.as_ref().map(|_| "<redacted>"),
            )
            .field("reconnect_interval", &self.config.reconnect_interval)
            .field("connect_timeout", &self.config.connect_timeout)
            .field("handshake_timeout", &self.config.handshake_timeout)
            .field("max_peers", &self.config.max_peers)
            .field("max_queue_messages", &self.config.max_queue_messages)
            .field("max_queue_bytes", &self.config.max_queue_bytes)
            .field("max_streams", &self.config.max_streams)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpVodPlan {
    limits: VodLimits,
    sources: Vec<VodSourceDefinition>,
    outbound_policy: RtmpOutboundPolicy,
}
impl RtmpVodPlan {
    pub fn new(
        limits: VodLimits,
        sources: impl IntoIterator<Item = VodSourceDefinition>,
        outbound_policy: RtmpOutboundPolicy,
    ) -> Result<Self, RtmpPrepareError> {
        outbound_policy
            .validate_cidrs_intrinsic()
            .map_err(|source| value("vod.outbound_policy.cidrs").source(source))?;
        limits
            .validate_intrinsic()
            .map_err(|source| bound("vod.limits").source(source))?;
        let sources: Vec<_> = sources.into_iter().collect();
        for source in &sources {
            source
                .validate_intrinsic()
                .map_err(|source| value("vod.sources").source(source))?;
        }
        Ok(Self {
            limits,
            sources,
            outbound_policy,
        })
    }
    #[must_use]
    pub const fn limits(&self) -> VodLimits {
        self.limits
    }
    #[must_use]
    pub fn sources(&self) -> &[VodSourceDefinition] {
        &self.sources
    }
    #[must_use]
    pub const fn outbound_policy(&self) -> &RtmpOutboundPolicy {
        &self.outbound_policy
    }
    /// Opens local roots and acquires HTTP origins for one VOD application.
    ///
    /// # Errors
    ///
    /// Returns an acquisition error when a source root or origin cannot be prepared.
    pub(crate) fn acquire(
        &self,
        service: impl Into<String>,
        application: impl Into<String>,
    ) -> Result<crate::VodApplication, crate::VodError> {
        let blueprint = crate::VodApplicationBlueprint::compile(
            service,
            application,
            self.limits,
            self.sources.clone(),
            &self.outbound_policy,
        )?;
        crate::VodApplication::acquire(&blueprint)
    }
}
impl fmt::Debug for RtmpVodPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtmpVodPlan")
            .field("limits", &self.limits)
            .field(
                "sources",
                &format_args!("<{} redacted>", self.sources.len()),
            )
            .field("outbound_policy", &self.outbound_policy)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpCallbackPlan {
    endpoints: [Option<String>; 8],
    method: RtmpCallbackMethod,
    timeout: Duration,
    update_timeout: Duration,
    update_strict: bool,
    relay_redirect: bool,
}
impl Default for RtmpCallbackPlan {
    fn default() -> Self {
        Self {
            endpoints: Default::default(),
            method: RtmpCallbackMethod::default(),
            timeout: Duration::from_secs(10),
            update_timeout: Duration::from_secs(30),
            update_strict: false,
            relay_redirect: false,
        }
    }
}
impl RtmpCallbackPlan {
    pub fn new(
        method: RtmpCallbackMethod,
        timeout: Duration,
        update_timeout: Duration,
    ) -> Result<Self, RtmpPrepareError> {
        if timeout.is_zero() {
            return Err(bound("callbacks.timeout"));
        }
        Ok(Self {
            method,
            timeout,
            update_timeout,
            ..Self::default()
        })
    }
    pub fn with_endpoint(
        mut self,
        event: RtmpCallbackEventPlan,
        url: impl Into<String>,
    ) -> Result<Self, RtmpPrepareError> {
        let url = url.into();
        validate_callback_url_intrinsic(&url)
            .map_err(|source| value("callbacks.url").source(source))?;
        self.endpoints[event as usize] = Some(url);
        Ok(self)
    }
    #[must_use]
    pub const fn with_update_policy(mut self, strict: bool, relay_redirect: bool) -> Self {
        self.update_strict = strict;
        self.relay_redirect = relay_redirect;
        self
    }
    #[must_use]
    pub fn endpoint(&self, event: RtmpCallbackEventPlan) -> Option<&str> {
        self.endpoints[event as usize].as_deref()
    }
    /// Parses, resolves, and policy-checks one configured callback endpoint.
    ///
    /// # Errors
    ///
    /// Returns a redacted callback error when the endpoint is malformed, cannot be resolved, or
    /// fails the configured outbound destination policy.
    pub(crate) fn acquire_endpoint(
        &self,
        event: RtmpCallbackEventPlan,
        outbound_policy: &RtmpOutboundPolicy,
    ) -> Result<Option<crate::RtmpCallbackEndpoint>, crate::RtmpCallbackError> {
        self.endpoint(event)
            .map(|value| crate::RtmpCallbackEndpoint::parse(value, outbound_policy))
            .transpose()
    }
    #[must_use]
    pub const fn method(&self) -> RtmpCallbackMethod {
        self.method
    }
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
    #[must_use]
    pub const fn update_timeout(&self) -> Duration {
        self.update_timeout
    }
    #[must_use]
    pub const fn update_strict(&self) -> bool {
        self.update_strict
    }
    #[must_use]
    pub const fn relay_redirect(&self) -> bool {
        self.relay_redirect
    }
}
impl fmt::Debug for RtmpCallbackPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let endpoints = self
            .endpoints
            .each_ref()
            .map(|value| value.as_ref().map(|_| "<redacted>"));
        f.debug_struct("RtmpCallbackPlan")
            .field("endpoints", &endpoints)
            .field("method", &self.method)
            .field("timeout", &self.timeout)
            .field("update_timeout", &self.update_timeout)
            .field("update_strict", &self.update_strict)
            .field("relay_redirect", &self.relay_redirect)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum RtmpCallbackEventPlan {
    Connect,
    Disconnect,
    Publish,
    PublishDone,
    Play,
    PlayDone,
    Done,
    Update,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpExecPlan {
    profile: ExecProfile,
}
impl RtmpExecPlan {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: impl Into<String>,
        application: impl Into<String>,
        mode: ExecMode,
        trigger: ExecTrigger,
        executable: PathBuf,
        arguments: impl IntoIterator<Item = String>,
        environment: impl IntoIterator<Item = RtmpExecEnvironmentPlan>,
        working_directory: PathBuf,
        filesystem: ExecFilesystemPolicy,
        network: ExecNetworkPolicy,
        limits: ExecLimits,
        respawn: bool,
    ) -> Result<Self, RtmpPrepareError> {
        let name = name.into();
        let environment = environment
            .into_iter()
            .map(|value| value.entry)
            .collect::<Vec<_>>();
        let profile = ExecProfile::new(
            name.clone(),
            application,
            mode,
            trigger,
            executable,
            arguments,
            environment,
            working_directory,
            filesystem,
            network,
            limits,
            respawn,
        )
        .map_err(|error| exec_error(&error).with_profile(name).source(error))?;
        Ok(Self { profile })
    }
    #[must_use]
    pub fn name(&self) -> &str {
        self.profile.name()
    }
    #[must_use]
    pub fn application(&self) -> &str {
        self.profile.application()
    }
    #[must_use]
    pub const fn profile(&self) -> &ExecProfile {
        &self.profile
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpExecEnvironmentPlan {
    entry: ExecEnvironment,
}
impl RtmpExecEnvironmentPlan {
    pub fn new(
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, RtmpPrepareError> {
        Ok(Self {
            entry: ExecEnvironment::new(name, value)
                .map_err(|error| exec_error(&error).source(error))?,
        })
    }
    #[must_use]
    pub fn name(&self) -> &str {
        self.entry.name()
    }
    #[must_use]
    pub fn value(&self) -> &str {
        self.entry.value()
    }
}
impl fmt::Debug for RtmpExecEnvironmentPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.entry.fmt(f)
    }
}
