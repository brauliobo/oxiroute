//! Safe Unix transport and descriptor validation for service supervision.
//!
//! This crate deliberately does not spawn processes or integrate with a server runtime.

#[cfg(not(target_os = "linux"))]
compile_error!("oxiroute-supervision-unix requires Linux");

#[cfg(target_os = "linux")]
mod descriptor;
#[cfg(target_os = "linux")]
mod identity;
#[cfg(target_os = "linux")]
mod transport;

#[cfg(target_os = "linux")]
pub use descriptor::{
    BindIdentity, DescriptorCapabilities, DescriptorError, DescriptorKind, DescriptorManifest,
    DescriptorRole, DescriptorSet, DescriptorSlot, SlotId,
};
#[cfg(target_os = "linux")]
pub use identity::{PeerIdentity, SpawnHandshakeNonce};
#[cfg(target_os = "linux")]
pub use transport::{
    FRAME_HEADER_SIZE, Frame, FrameFlags, FrameHeader, InstanceToken, MAX_DESCRIPTOR_COUNT,
    MAX_FRAME_SIZE, MAX_PAYLOAD_SIZE, MessageType, SeqpacketEndpoint, TransportError,
};
