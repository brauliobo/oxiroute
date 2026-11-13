//! Lease-based dynamic server registration.

pub mod auth;
#[cfg(test)]
mod contract;
pub mod dto;
pub mod error;
pub(crate) mod host;
pub mod http;
pub mod lease;
pub mod limits;
pub(crate) mod listener;
pub mod record;
pub mod service;
pub(crate) mod status_feed;
pub mod store;
pub mod validate;
