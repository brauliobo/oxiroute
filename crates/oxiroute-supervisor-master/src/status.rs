use std::collections::{BTreeMap, VecDeque};

use oxiroute_supervision::{
    BoundedWireProtocol, BoundedWireReader, BoundedWireWriter, GenerationId, GenerationRole,
    InstanceId, SupervisedGenerationCatalog,
};
use oxiroute_supervision_unix::MessageType;
use oxiroute_supervisor_process::AuthenticatedFrame;
use thiserror::Error;

use crate::CONTROL_PROTOCOL_VERSION;

pub(crate) const STATUS_MESSAGE: MessageType = MessageType(0x181);
/// Maximum encoded worker status bytes accepted by either endpoint.
pub const MAX_STATUS_BYTES: usize = 60 * 1024;
/// Maximum listener observations carried by one worker status.
pub const MAX_STATUS_LISTENERS: usize = 64;
/// Maximum event observations carried by one worker status.
pub const MAX_STATUS_EVENTS: usize = 64;
/// Maximum worker-originated events retained by the master.
pub const MAX_AGGREGATED_EVENTS: usize = 2_048;

const MAX_STATUS_STRING_BYTES: usize = 1_024;
const STATUS_FORMAT_VERSION: u16 = 1;

mod history;
mod model;
mod wire;

pub(crate) use history::*;
pub use model::*;
pub use wire::*;

#[cfg(test)]
mod tests;
