//! HTTP/1 head codec of the compiled fast lane.

#![no_std]

extern crate alloc;

mod class;
mod edit;
mod emit;
mod fields;
mod miss;
mod order;
mod request;
mod response;
mod tables;
mod write_request;
mod write_response;

pub use class::{Class, classify};
pub use class::{
    HOP_BY_HOP_REQUEST, HOP_BY_HOP_RESPONSE, HOP_CONNECTION, HOP_KEEP_ALIVE, HOP_PROXY_AUTHENTICATE,
};
pub use class::{
    HOP_PROXY_AUTHORIZATION, HOP_PROXY_CONNECTION, HOP_TE, HOP_TRAILER, HOP_TRANSFER_ENCODING,
    HOP_UPGRADE,
};
pub use edit::{Edit, Edits, EditsFull, MAX_EDITS, Name, Value, names};
pub use fields::{
    Header, LINE_IS_CLEAN, MAX_HEADERS, MAX_NAME_LEN, NAME_HAS_UNDERSCORE, NAME_HAS_UPPER,
};
pub use miss::{MissReason, Scan, ScanResponse};
pub use order::{HeaderOrder, Items, MAX_ITEMS};
pub use request::*;
pub use response::{ResponseFacts, ResponseHead, scan_response};
pub use tables::is_header_value;
pub use write_request::write_request;
pub use write_response::{Automatic, write_response};
