use super::{ControlAck, ExitStatus, InstanceId, WorkerStatus};

#[derive(Clone)]
pub(super) enum Observation {
    Ack(ControlAck),
    Status(Box<WorkerStatus>),
    Exit(ExitStatus),
    Disconnected,
    ProtocolFailure,
}

pub(super) struct Observed {
    pub(super) instance_id: InstanceId,
    pub(super) observation: Observation,
}
