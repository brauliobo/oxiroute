/// Optional storage quotas and the hard active-recorder bound for one pinned recording root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordingStoreLimits {
    pub max_bytes: Option<u64>,
    pub max_files: Option<usize>,
    pub max_active_recorders: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("recording store limit `{field}` must be nonzero when configured")]
pub struct RecordingStoreLimitsError {
    pub field: &'static str,
}

impl RecordingStoreLimits {
    /// Validates quota values without opening or inspecting a recording root.
    ///
    /// # Errors
    ///
    /// Returns an error when the active-recorder bound cannot initialize the store finalizer.
    pub fn validate_intrinsic(self) -> Result<(), RecordingStoreLimitsError> {
        if self.max_active_recorders == 0 {
            return Err(RecordingStoreLimitsError {
                field: "max_active_recorders",
            });
        }
        Ok(())
    }
}

/// Current use under one recording store.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RecordingStoreStats {
    pub bytes_used: u64,
    pub files: usize,
    pub active_recorders: usize,
}

/// Quota counters are shared by stores in this process. The lock protocol protects partial cleanup
/// across processes, but quota counters are not distributed between daemon processes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordingQuotaScope {
    Process,
}
