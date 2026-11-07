use std::net::IpAddr;

mod forms;
mod model;
mod table;
mod validate;

pub use model::*;
use table::DIRECTIVES;
pub use validate::*;

use DirectiveContext::{Http, NginxMain, RtmpApplication, RtmpMain, RtmpRecorder, RtmpServer};
use DirectiveStatus::{
    Deprecated as StatusDeprecated, DisableOnly, Enforced, ParsedOnly,
    PlatformLimited as StatusPlatformLimited, SourceBug as StatusSourceBug,
    SourceNoOp as StatusSourceNoOp,
};
use RuntimeSupport::{Deprecated, ParsedNotEnforced, PlatformLimited, SourceBug, SourceNoOp};

#[cfg(test)]
mod tests;
