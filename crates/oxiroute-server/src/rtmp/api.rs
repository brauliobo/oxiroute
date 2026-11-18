#![allow(clippy::result_large_err)]

mod auth;
mod config;
#[cfg(test)]
mod contract;
pub(crate) use config::read_config_body;
pub(crate) mod dto;
mod endpoint_registry;
mod management;
mod media;
mod observability;
mod recordings;
pub(crate) mod response;
mod route;
mod rtmp;
mod service;
mod streams;
mod ticket;
mod ui;
mod vod;
mod wire;

pub use self::{
    auth::management_token_file,
    response::ApiResponse,
    service::{RtmpManagementApi, RtmpManagementHttpApp},
};
pub(crate) use self::{
    auth::{TOKEN_FILE_ENV, preflight_management_token, token_file_or},
    ticket::{export_media_ticket_secret, import_media_ticket_secret},
    ui::UiAssets,
};

pub const MAX_CONFIG_REQUEST_BYTES: usize = crate::config::coordinator::MAX_CANONICAL_CONFIG_BYTES;
