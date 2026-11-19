#[allow(clippy::wildcard_imports)]
use super::*;

const PROTOCOL: u32 = 1;
const NO_INSTANCE: InstanceToken = InstanceToken([0; 16]);

/// What flows between a master and the master that succeeds it, over their private channel.
pub(super) trait Message: Serialize + DeserializeOwned {
    const KIND: MessageType;
}

/// The listeners, as the manifest that describes their descriptors in order.
#[derive(Deserialize, Serialize)]
pub(super) struct Offer {
    pub(super) protocol: u32,
    pub(super) manifest: DescriptorManifest,
}

impl Offer {
    pub(super) fn new(manifest: DescriptorManifest) -> Self {
        Self {
            protocol: PROTOCOL,
            manifest,
        }
    }

    pub(super) fn check(&self) -> Result<(), String> {
        if self.protocol == PROTOCOL {
            Ok(())
        } else {
            Err(format!(
                "the previous master speaks hand-over protocol {}, this one {PROTOCOL}",
                self.protocol
            ))
        }
    }
}

impl Message for Offer {
    const KIND: MessageType = MessageType(0x4801);
}

/// The successor answers once, when it serves or when it cannot.
#[derive(Deserialize, Serialize)]
pub(super) enum Reply {
    Ready,
    Failed { reason: String },
}

impl Message for Reply {
    const KIND: MessageType = MessageType(0x4802);
}

/// The previous master no longer takes upgrade requests: the successor may.
#[derive(Deserialize, Serialize)]
pub(super) struct Commit;

impl Message for Commit {
    const KIND: MessageType = MessageType(0x4803);
}

pub(super) fn send<T: Message>(
    endpoint: &mut SeqpacketEndpoint,
    message: &T,
    descriptors: &[BorrowedFd<'_>],
) -> Result<(), Box<dyn Error>> {
    endpoint.send(
        T::KIND,
        FrameFlags::default(),
        NO_INSTANCE,
        GenerationId(0),
        &serde_json::to_vec(message)?,
        descriptors,
    )?;
    Ok(())
}

pub(super) fn receive<T: Message>(
    endpoint: &mut SeqpacketEndpoint,
) -> Result<(T, Vec<OwnedFd>), Box<dyn Error>> {
    let (header, payload, descriptors, _) = endpoint.receive()?.into_parts();
    if header.message_type() != T::KIND {
        return Err(format!("unexpected hand-over message {:?}", header.message_type()).into());
    }
    Ok((serde_json::from_slice(&payload)?, descriptors))
}
