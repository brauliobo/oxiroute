mod config;
mod observability;
mod response;
mod service;
mod streams;
mod ui;

pub use self::{response::ApiResponse, service::RtmpManagementApi};

pub const MAX_CONFIG_REQUEST_BYTES: usize = crate::config_coordinator::MAX_CANONICAL_CONFIG_BYTES;
