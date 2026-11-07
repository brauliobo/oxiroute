//! Lease-based dynamic server registration. See `docs/developer/design/DYNAMIC_REGISTRY.md`.

pub mod auth;
pub mod error;
pub(crate) mod host;
pub mod lease;
pub mod limits;
pub mod record;
pub mod service;
pub mod store;
pub mod validate;
