#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) enum ListenerSource<'a> {
    BindOrReuse {
        previous: Option<&'a ListenerReservations>,
        process: ProcessRuntime,
    },
    #[cfg(target_os = "linux")]
    Adopt {
        descriptors: DescriptorSet,
        process: ProcessRuntime,
    },
    Validate {
        previous: Option<&'a ListenerReservations>,
    },
}

#[expect(
    clippy::large_enum_variant,
    reason = "the private output transfers one complete prepared generation without another owner"
)]
pub(super) enum PreparationOutput {
    Prepared(PreparedGeneration),
    Validated,
}

pub(super) struct PreparedGenerationTransaction {
    #[cfg(test)]
    pub(super) drop_probes: Vec<PreparedTransactionDropProbe>,
    pub(super) reservations_precede_plan: bool,
    #[cfg(target_os = "linux")]
    pub(super) descriptors: Option<DescriptorSet>,
    pub(super) rtmp: Option<PreparedRtmpRuntimeSet>,
    pub(super) listener_registration: Option<crate::monitoring::ListenerRegistrationTransaction>,
    pub(super) reservations: Option<ListenerReservations>,
    pub(super) acquired: Option<crate::service_plan::GenerationAcquisition>,
    pub(super) plan: Option<RuntimePlan>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PreparedTransactionFault {
    Reservations,
    ListenerRegistration,
    RtmpPreparation,
}

#[cfg(test)]
std::thread_local! {
    static PREPARED_TRANSACTION_FAULT: std::cell::Cell<Option<PreparedTransactionFault>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn with_prepared_transaction_fault<T>(
    fault: PreparedTransactionFault,
    run: impl FnOnce() -> T,
) -> T {
    PREPARED_TRANSACTION_FAULT.set(Some(fault));
    let result = run();
    PREPARED_TRANSACTION_FAULT.set(None);
    result
}

#[cfg(test)]
pub(super) fn fail_after_prepared_transaction_stage(
    stage: PreparedTransactionFault,
) -> Result<(), GenerationError> {
    if PREPARED_TRANSACTION_FAULT.get() == Some(stage) {
        return Err(GenerationError::RuntimePrepare);
    }
    Ok(())
}

#[cfg(not(test))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "production and test builds share preparation boundary-fault call sites"
)]
pub(super) fn fail_after_prepared_transaction_stage(_stage: ()) -> Result<(), GenerationError> {
    Ok(())
}

#[cfg(test)]
std::thread_local! {
    pub(super) static PREPARED_TRANSACTION_ROLLBACK_TRACE: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl PreparedGenerationTransaction {
    pub(super) fn new() -> Self {
        Self {
            #[cfg(test)]
            drop_probes: Vec::new(),
            reservations_precede_plan: false,
            #[cfg(target_os = "linux")]
            descriptors: None,
            rtmp: None,
            listener_registration: None,
            reservations: None,
            acquired: None,
            plan: None,
        }
    }

    pub(super) fn acquired(&self) -> &crate::service_plan::GenerationAcquisition {
        self.acquired.as_ref().expect("provisional acquisition")
    }

    pub(super) fn commit(mut self) -> GenerationResources {
        #[cfg(test)]
        for probe in &mut self.drop_probes {
            probe.armed = false;
        }
        GenerationResources::commit(
            self.plan.take().expect("provisional runtime plan"),
            self.acquired.take().expect("provisional acquisition"),
            self.reservations.take().expect("provisional reservations"),
            self.listener_registration
                .take()
                .expect("provisional listener registration"),
            self.rtmp.take().expect("provisional RTMP runtime"),
        )
    }
}

impl Drop for PreparedGenerationTransaction {
    fn drop(&mut self) {
        self.rtmp.take();
        #[cfg(test)]
        drop_prepared_transaction_probe(&mut self.drop_probes, "rtmp_prepared");
        #[cfg(test)]
        self.listener_registration.take();
        #[cfg(test)]
        drop_prepared_transaction_probe(&mut self.drop_probes, "listener_registration");
        if self.reservations_precede_plan {
            self.acquired.take();
            #[cfg(test)]
            drop_prepared_transaction_probe(&mut self.drop_probes, "acquisition");
            self.plan.take();
            #[cfg(test)]
            drop_prepared_transaction_probe(&mut self.drop_probes, "plan");
            self.reservations.take();
            #[cfg(test)]
            drop_prepared_transaction_probe(&mut self.drop_probes, "reservations");
        } else {
            self.reservations.take();
            #[cfg(test)]
            drop_prepared_transaction_probe(&mut self.drop_probes, "reservations");
            self.acquired.take();
            #[cfg(test)]
            drop_prepared_transaction_probe(&mut self.drop_probes, "acquisition");
            self.plan.take();
            #[cfg(test)]
            drop_prepared_transaction_probe(&mut self.drop_probes, "plan");
        }
        #[cfg(target_os = "linux")]
        self.descriptors.take();
    }
}

#[cfg(test)]
pub(super) struct PreparedTransactionDropProbe {
    stage: &'static str,
    armed: bool,
}

#[cfg(test)]
impl PreparedTransactionDropProbe {
    pub(super) const fn new(stage: &'static str) -> Self {
        Self { stage, armed: true }
    }
}

#[cfg(test)]
impl Drop for PreparedTransactionDropProbe {
    fn drop(&mut self) {
        if self.armed {
            PREPARED_TRANSACTION_ROLLBACK_TRACE.with(|trace| trace.borrow_mut().push(self.stage));
        }
    }
}

#[cfg(test)]
fn drop_prepared_transaction_probe(
    probes: &mut Vec<PreparedTransactionDropProbe>,
    stage: &'static str,
) {
    if probes.last().is_some_and(|probe| probe.stage == stage) {
        drop(probes.pop());
    }
}
