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
mod tables;
mod write_request;

pub use class::{Class, classify};
pub use edit::{Edit, Edits, EditsFull, MAX_EDITS, Name, Value, names};
pub use fields::{
    Header, LINE_IS_CLEAN, MAX_HEADERS, MAX_NAME_LEN, NAME_HAS_UNDERSCORE, NAME_HAS_UPPER,
};
pub use miss::{MissReason, Scan, ScanResponse};
pub use order::{HeaderOrder, Items, MAX_ITEMS};
pub use request::*;
pub use tables::is_header_value;
pub use write_request::write_request;
