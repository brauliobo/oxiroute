#![allow(clippy::result_large_err)]

mod auth;
mod config;
pub(crate) use config::read_config_body;
pub(crate) mod dto;
mod endpoint_registry;
mod management;
mod media;
mod observability;
pub(crate) mod response;
mod route;
mod rtmp;
mod service;
mod streams;
mod ui;
mod vod;
mod wire;

pub(crate) use self::{auth::preflight_management_token, ui::UiAssets};
pub use self::{
    response::ApiResponse,
    service::{RtmpManagementApi, RtmpManagementHttpApp},
};

pub const MAX_CONFIG_REQUEST_BYTES: usize = crate::config_coordinator::MAX_CANONICAL_CONFIG_BYTES;
