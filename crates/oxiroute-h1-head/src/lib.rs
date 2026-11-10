//! HTTP/1 head codec of the compiled fast lane.

#![no_std]

extern crate alloc;

mod class;
mod fields;
mod miss;
mod request;
mod tables;

pub use class::{Class, classify};
pub use fields::{
    Header, LINE_IS_CLEAN, MAX_HEADERS, MAX_NAME_LEN, NAME_HAS_UNDERSCORE, NAME_HAS_UPPER,
};
pub use miss::{MissReason, Scan, ScanResponse};
pub use request::*;
pub use tables::is_header_value;
