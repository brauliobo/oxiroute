#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Debug, thiserror::Error)]
pub enum RecordingStoreError {
    #[error("recording root cannot be opened as a no-follow directory")]
    RootOpen(#[source] io::Error),
    #[error("recording root cannot be enumerated")]
    RootRead(#[source] io::Error),
    #[error("recording root entry cannot be inspected")]
    RootEntryMetadata(#[source] io::Error),
    #[error("recording root must be daemon-owned and not writable by group or other users")]
    RootNotExclusive,
    #[error("recording root ownership lease cannot be opened")]
    OwnershipOpen(#[source] io::Error),
    #[error("recording root ownership lease cannot be acquired")]
    OwnershipLock(#[source] io::Error),
    #[error("recording creation was cancelled while waiting for storage ownership")]
    CreationCancelled,
    #[error("owned recording partial cannot be removed")]
    PartialCleanup(#[source] io::Error),
    #[error("recording root cannot be synchronized")]
    RootSync(#[source] io::Error),
    #[error(
        "recording root already uses {bytes_used} bytes and {files} files, exceeding configured limits"
    )]
    ExistingUsageExceedsLimits { bytes_used: u64, files: usize },
    #[error("recording root is already open in this process with different limits")]
    LimitsMismatch,
    #[error("recording finalizer thread cannot be started")]
    FinalizerThreadSpawn(#[source] io::Error),
    #[error("recording final name must be a bounded relative path component")]
    InvalidRelativeName,
    #[error("recording active-recorder limit of {maximum} reached")]
    ActiveRecorderLimit { maximum: usize },
    #[error("recording file limit of {maximum} reached")]
    FileLimit { maximum: usize },
    #[error("recording partial-name collision retry limit reached")]
    PartialNameCollisions,
    #[error("recording partial cannot be created")]
    PartialCreate(#[source] io::Error),
    #[error("existing recording cannot be securely reopened")]
    ResumeOpen(#[source] io::Error),
    #[error("existing recording is not a complete FLV stream")]
    ResumeInvalid,
    #[error("recording partial {partial_relative_name} cannot be flushed or synchronized")]
    FileSync {
        partial_relative_name: String,
        #[source]
        source: io::Error,
    },
    #[error("recording partial ownership was lost before publication")]
    PartialOwnershipLost { partial_relative_name: String },
    #[error("recording finalization was cancelled before publication")]
    FinalizationCancelled { partial_relative_name: String },
    #[error("recording partial cannot be atomically published")]
    Publish {
        partial_relative_name: String,
        #[source]
        source: io::Error,
    },
    #[error("recording partial unlink failed and the published recording could not be rolled back")]
    PublishRollback {
        partial_relative_name: String,
        recording: RecordingCommit,
        #[source]
        source: io::Error,
        rollback_source: io::Error,
    },
    #[error("recording rollback could not be synchronized after the final name was removed")]
    PublishRollbackDirectorySync {
        partial_relative_name: String,
        recording: RecordingCommit,
        #[source]
        source: io::Error,
        directory_sync_source: io::Error,
    },
    #[error("descriptor-based recording publication is unsupported")]
    DescriptorPublicationUnsupported { partial_relative_name: String },
    #[error("recording final-name collision retry limit reached")]
    FinalNameCollisions { partial_relative_name: String },
    #[error("recording was published but its directory cannot be synchronized")]
    PublishedDirectorySync {
        recording: RecordingCommit,
        #[source]
        source: io::Error,
    },
}

impl RecordingStoreError {
    #[must_use]
    pub fn recoverable_partial_name(&self) -> Option<&str> {
        match self {
            Self::FileSync {
                partial_relative_name,
                ..
            }
            | Self::FinalizationCancelled {
                partial_relative_name,
            }
            | Self::Publish {
                partial_relative_name,
                ..
            }
            | Self::PublishRollback {
                partial_relative_name,
                ..
            }
            | Self::PublishRollbackDirectorySync {
                partial_relative_name,
                ..
            }
            | Self::DescriptorPublicationUnsupported {
                partial_relative_name,
            }
            | Self::FinalNameCollisions {
                partial_relative_name,
            } => Some(partial_relative_name),
            _ => None,
        }
    }

    #[must_use]
    pub const fn published_recording(&self) -> Option<&RecordingCommit> {
        match self {
            Self::PublishRollback { recording, .. }
            | Self::PublishRollbackDirectorySync { recording, .. }
            | Self::PublishedDirectorySync { recording, .. } => Some(recording),
            _ => None,
        }
    }
}
