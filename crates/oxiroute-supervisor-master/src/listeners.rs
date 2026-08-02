use std::{io, os::fd::OwnedFd};

use oxiroute_supervision_unix::{DescriptorError, DescriptorManifest, DescriptorSet};
use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};
use thiserror::Error;

/// Stable manifest and original listener ownership retained by the master.
#[derive(Debug)]
pub struct StableListeners {
    manifest: DescriptorManifest,
    originals: Vec<OwnedFd>,
}

impl StableListeners {
    /// Validates the original descriptors through temporary duplicates, then retains the originals.
    ///
    /// # Errors
    ///
    /// Returns an error when duplication fails or descriptors do not exactly satisfy the manifest.
    pub fn new(
        manifest: DescriptorManifest,
        originals: Vec<OwnedFd>,
    ) -> Result<Self, ListenerOwnershipError> {
        if originals.len() != manifest.slots().len() {
            return Err(DescriptorError::CardinalityMismatch {
                expected: manifest.slots().len(),
                actual: originals.len(),
            }
            .into());
        }
        for original in &originals {
            let flags = fcntl_getfd(original).map_err(io::Error::from)?;
            if !flags.contains(FdFlags::CLOEXEC) {
                fcntl_setfd(original, flags | FdFlags::CLOEXEC).map_err(io::Error::from)?;
            }
            if !fcntl_getfd(original)
                .map_err(io::Error::from)?
                .contains(FdFlags::CLOEXEC)
            {
                return Err(ListenerOwnershipError::CloexecUnavailable);
            }
        }
        let validation = duplicate_all(&originals)?;
        DescriptorSet::new(&manifest, validation)?;
        Ok(Self {
            manifest,
            originals,
        })
    }

    /// Returns the stable listener manifest.
    #[must_use]
    pub const fn manifest(&self) -> &DescriptorManifest {
        &self.manifest
    }

    /// Returns the number of original descriptors retained by the master.
    #[must_use]
    pub fn len(&self) -> usize {
        self.originals.len()
    }

    /// Returns whether no listener descriptors are owned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.originals.is_empty()
    }

    pub(crate) fn duplicates(&self) -> Result<Vec<OwnedFd>, ListenerOwnershipError> {
        duplicate_all(&self.originals)
    }
}

fn duplicate_all(descriptors: &[OwnedFd]) -> Result<Vec<OwnedFd>, ListenerOwnershipError> {
    descriptors
        .iter()
        .map(|descriptor| {
            rustix::io::fcntl_dupfd_cloexec(descriptor, 0)
                .map_err(io::Error::from)
                .map_err(ListenerOwnershipError::Duplicate)
        })
        .collect()
}

/// Listener validation or duplication failure.
#[derive(Debug, Error)]
pub enum ListenerOwnershipError {
    /// Original listener flags could not be inspected or changed.
    #[error("failed to enforce CLOEXEC on a master-owned listener: {0}")]
    OriginalFlags(#[from] io::Error),
    /// An original listener remained inheritable after setting `CLOEXEC`.
    #[error("master-owned listener could not be made CLOEXEC")]
    CloexecUnavailable,
    /// A `CLOEXEC` duplicate could not be created.
    #[error("failed to duplicate a master-owned listener: {0}")]
    Duplicate(io::Error),
    /// The descriptors did not satisfy their manifest.
    #[error(transparent)]
    Descriptor(#[from] DescriptorError),
}
