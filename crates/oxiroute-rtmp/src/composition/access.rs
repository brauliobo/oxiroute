#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RtmpAccessPlan {
    rules: Vec<RtmpAccessRulePlan>,
    token: Option<RtmpTokenPlan>,
}

impl RtmpAccessPlan {
    #[must_use]
    pub fn new(
        rules: impl IntoIterator<Item = RtmpAccessRulePlan>,
        token: Option<RtmpTokenPlan>,
    ) -> Self {
        Self {
            rules: rules.into_iter().collect(),
            token,
        }
    }
    #[must_use]
    pub fn rules(&self) -> &[RtmpAccessRulePlan] {
        &self.rules
    }
    #[must_use]
    pub const fn token(&self) -> Option<&RtmpTokenPlan> {
        self.token.as_ref()
    }

    #[must_use]
    pub fn runtime_policy(&self) -> RtmpAccessPolicy {
        RtmpAccessPolicy::new(
            self.rules
                .iter()
                .map(|rule| RtmpAccessRule::new(rule.action(), rule.network().clone())),
            self.token.as_ref().map(|token| token.policy().clone()),
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpAccessRulePlan {
    rule: RtmpAccessRule,
}

impl RtmpAccessRulePlan {
    pub fn new(action: RtmpAccessAction, network: RtmpNetwork) -> Result<Self, RtmpPrepareError> {
        if !network.is_representable() {
            return Err(value("access.network"));
        }
        Ok(Self {
            rule: RtmpAccessRule::new(action, network),
        })
    }
    #[must_use]
    pub const fn action(&self) -> RtmpAccessAction {
        self.rule.action()
    }
    #[must_use]
    pub const fn network(&self) -> &RtmpNetwork {
        self.rule.network()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RtmpTokenPlan {
    policy: RtmpTokenPolicy,
}

impl RtmpTokenPlan {
    pub fn new(
        parameter: impl Into<String>,
        secret: impl AsRef<[u8]>,
    ) -> Result<Self, RtmpPrepareError> {
        let parameter = parameter.into();
        let policy = RtmpTokenPolicy::stream_query(parameter, secret)
            .ok_or_else(|| value("access.token"))?;
        Ok(Self { policy })
    }
    #[must_use]
    pub fn parameter(&self) -> &str {
        self.policy.parameter()
    }
    #[must_use]
    pub const fn policy(&self) -> &RtmpTokenPolicy {
        &self.policy
    }
}

impl fmt::Debug for RtmpTokenPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtmpTokenPlan")
            .field("parameter", &self.parameter())
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[allow(clippy::struct_field_names)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtmpFanoutPlan {
    max_subscribers: usize,
    max_queue_messages_per_subscriber: usize,
    max_queue_bytes_per_subscriber: usize,
}

impl RtmpFanoutPlan {
    pub fn new(
        max_subscribers: usize,
        max_queue_messages_per_subscriber: usize,
        max_queue_bytes_per_subscriber: usize,
    ) -> Result<Self, RtmpPrepareError> {
        if max_subscribers == 0
            || max_queue_messages_per_subscriber == 0
            || max_queue_bytes_per_subscriber == 0
            || max_subscribers
                .checked_mul(max_queue_bytes_per_subscriber)
                .is_none()
        {
            return Err(bound("fanout"));
        }
        Ok(Self {
            max_subscribers,
            max_queue_messages_per_subscriber,
            max_queue_bytes_per_subscriber,
        })
    }
    #[must_use]
    pub const fn max_subscribers(self) -> usize {
        self.max_subscribers
    }
    #[must_use]
    pub const fn max_queue_messages_per_subscriber(self) -> usize {
        self.max_queue_messages_per_subscriber
    }
    #[must_use]
    pub const fn max_queue_bytes_per_subscriber(self) -> usize {
        self.max_queue_bytes_per_subscriber
    }

    /// Builds the live fanout hub represented by this validated plan.
    #[must_use]
    pub(crate) fn runtime_hub(self) -> LiveHub {
        LiveHub::new(LiveHubLimits {
            max_subscribers: self.max_subscribers,
            max_subscribers_per_stream: self.max_subscribers,
            max_queue_messages_per_subscriber: self.max_queue_messages_per_subscriber,
            max_queue_bytes_per_subscriber: self.max_queue_bytes_per_subscriber,
            max_fanout_bytes: self.max_subscribers * self.max_queue_bytes_per_subscriber,
            ..LiveHubLimits::default()
        })
    }
}
