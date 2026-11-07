use std::time::Duration;

/// Every way a registry operation can be refused, with its HTTP status and stable code.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RegistryError {
    #[error("{0}")]
    InvalidRequest(&'static str),
    #[error("server names are 1 to 64 characters of letters, digits, `.`, `_` and `-`")]
    InvalidName,
    #[error("{0}")]
    InvalidAddress(&'static str),
    #[error("ttlMs is outside the pool's bounds")]
    InvalidTtl,
    #[error("labels or metadata exceed their limits")]
    InvalidMetadata,
    #[error("a valid bearer token is required")]
    Unauthorized,
    #[error("the credential does not cover this pool or server")]
    ScopeDenied,
    #[error("{0}")]
    AddressNotAllowed(&'static str),
    #[error("this endpoint does not accept requests from the peer address")]
    PeerNotAllowed,
    #[error("no such route")]
    RouteNotFound,
    #[error("the pool does not exist")]
    PoolNotFound,
    #[error("no registration with that name; register again")]
    RegistrationNotFound,
    #[error("the pool does not accept registrations")]
    PoolNotDynamic,
    #[error("a static server already uses that name")]
    NameConflict,
    #[error("another server in the pool already uses that address")]
    EndpointConflict,
    #[error("the name is leased to a different address")]
    LeaseHeld,
    #[error("the pool reached its registration limit")]
    PoolFull,
    #[error("this generation no longer owns registrations; reconnect")]
    GenerationInactive,
    #[error("the request body is too large")]
    BodyTooLarge,
    #[error("too many requests")]
    RateLimited { retry_after: Duration },
    #[error("the registration store is unavailable")]
    RegistryUnavailable,
    #[error("no generation is active")]
    GenerationUnavailable,
}

impl RegistryError {
    pub fn status(&self) -> u16 {
        match self {
            Self::InvalidRequest(_)
            | Self::InvalidName
            | Self::InvalidAddress(_)
            | Self::InvalidTtl
            | Self::InvalidMetadata => 400,
            Self::Unauthorized => 401,
            Self::ScopeDenied | Self::AddressNotAllowed(_) | Self::PeerNotAllowed => 403,
            Self::RouteNotFound | Self::PoolNotFound | Self::RegistrationNotFound => 404,
            Self::PoolNotDynamic
            | Self::NameConflict
            | Self::EndpointConflict
            | Self::LeaseHeld
            | Self::PoolFull
            | Self::GenerationInactive => 409,
            Self::BodyTooLarge => 413,
            Self::RateLimited { .. } => 429,
            Self::RegistryUnavailable | Self::GenerationUnavailable => 503,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "invalid_request",
            Self::InvalidName => "invalid_name",
            Self::InvalidAddress(_) => "invalid_address",
            Self::InvalidTtl => "invalid_ttl",
            Self::InvalidMetadata => "invalid_metadata",
            Self::Unauthorized => "unauthorized",
            Self::ScopeDenied => "scope_denied",
            Self::AddressNotAllowed(_) => "address_not_allowed",
            Self::PeerNotAllowed => "peer_not_allowed",
            Self::RouteNotFound => "route_not_found",
            Self::PoolNotFound => "pool_not_found",
            Self::RegistrationNotFound => "registration_not_found",
            Self::PoolNotDynamic => "pool_not_dynamic",
            Self::NameConflict => "name_conflict",
            Self::EndpointConflict => "endpoint_conflict",
            Self::LeaseHeld => "lease_held",
            Self::PoolFull => "pool_full",
            Self::GenerationInactive => "generation_inactive",
            Self::BodyTooLarge => "body_too_large",
            Self::RateLimited { .. } => "rate_limited",
            Self::RegistryUnavailable => "registry_unavailable",
            Self::GenerationUnavailable => "generation_unavailable",
        }
    }
}
