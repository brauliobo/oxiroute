#![allow(clippy::result_large_err)]

mod config;
pub(crate) use config::read_config_body;
mod management;
mod media;
mod observability;
mod rtmp;
pub(crate) mod response;
mod service;
mod streams;
mod ui;
mod vod;

pub(crate) use self::{config::preflight_management_token, ui::UiAssets};
pub use self::{
    response::ApiResponse,
    service::{RtmpManagementApi, RtmpManagementHttpApp},
};

pub const MAX_CONFIG_REQUEST_BYTES: usize = crate::config_coordinator::MAX_CANONICAL_CONFIG_BYTES;
