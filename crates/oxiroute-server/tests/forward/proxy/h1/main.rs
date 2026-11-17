#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "../../../support/config.rs"]
mod config_support;
#[path = "../../../support/fixtures.rs"]
mod fixture_support;
#[path = "../../../support/process.rs"]
mod process_support;
#[path = "../../../support/mod.rs"]
mod support;

mod basic_auth;
mod cache;
mod common;
mod connect_udp;
mod downstream_tls;
mod peers;
mod squid;
