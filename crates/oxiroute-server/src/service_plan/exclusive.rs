use std::{path::PathBuf, sync::Arc};

use oxiroute_cache::DiskCacheError;

use crate::{AcmeManagedReconciler, TlsBuildError, http::cache::DiskBackend};

#[derive(Debug, thiserror::Error)]
pub enum StoreOwnershipError {
    #[error(
        "disk cache store `{store}` at `{}` is owned by the active worker; \
         the replacement takes it when the active worker releases it",
        root.display()
    )]
    DiskOwned { store: String, root: PathBuf },
    #[error("disk cache store `{store}` at `{}` cannot be claimed", root.display())]
    Disk {
        store: String,
        root: PathBuf,
        #[source]
        source: DiskCacheError,
    },
    #[error(transparent)]
    Acme(Box<TlsBuildError>),
}

/// The stores of one generation that only one writer may own at a time: persistent cache roots and
/// managed ACME state roots. A generation prepares with them unclaimed; whoever activates it claims
/// them after the generation it replaces released them.
#[derive(Clone, Default)]
pub struct ExclusiveStores {
    disks: Vec<Arc<DiskBackend>>,
    acme: Vec<Arc<AcmeManagedReconciler>>,
}

impl ExclusiveStores {
    pub(crate) fn new(disks: Vec<Arc<DiskBackend>>, acme: Vec<Arc<AcmeManagedReconciler>>) -> Self {
        Self { disks, acme }
    }

    /// Takes every store, or none: a store that cannot be taken releases the ones taken before it.
    ///
    /// # Errors
    ///
    /// Names the store that another owner still holds, or that cannot be opened.
    pub fn claim(&self) -> Result<(), StoreOwnershipError> {
        self.claim_in_order().inspect_err(|_| {
            let _ = self.release();
        })
    }

    /// Stops this generation from writing to its stores, so a replacement can take them. Resident
    /// cache entries keep serving hits; managed ACME renewal and issuance stop, and a running job
    /// is cancelled and finished first.
    ///
    /// # Errors
    ///
    /// Names the disk cache store whose ownership lease cannot be released.
    pub fn release(&self) -> Result<(), StoreOwnershipError> {
        for certificate in &self.acme {
            certificate.release_state();
        }
        self.disks.iter().try_for_each(|disk| {
            disk.release().map_err(|source| StoreOwnershipError::Disk {
                store: disk.store_name().to_owned(),
                root: disk.root().to_owned(),
                source,
            })
        })
    }

    /// Takes the stores back after a replacement that failed to activate, waiting while it still
    /// holds them. The caller's stores were released earlier, so their resident cache entries are
    /// reconciled with whatever the replacement changed on disk.
    ///
    /// # Errors
    ///
    /// Names the store that cannot be locked or scanned.
    pub fn reclaim(&self) -> Result<(), StoreOwnershipError> {
        self.disks.iter().try_for_each(|disk| {
            disk.reclaim().map_err(|source| StoreOwnershipError::Disk {
                store: disk.store_name().to_owned(),
                root: disk.root().to_owned(),
                source,
            })
        })?;
        self.acme.iter().try_for_each(|certificate| {
            certificate
                .reclaim_state()
                .map_err(|source| StoreOwnershipError::Acme(Box::new(source)))
        })
    }

    /// Moves the stores from the generation being replaced to its replacement: the previous
    /// owner releases first, and a replacement that cannot claim gives them back.
    ///
    /// # Errors
    ///
    /// Names the store the replacement could not claim.
    pub fn hand_over(previous: Option<&Self>, candidate: &Self) -> Result<(), StoreOwnershipError> {
        if let Some(previous) = previous {
            previous.release()?;
        }
        candidate.claim().inspect_err(|_| restore(previous))
    }

    /// Undoes [`Self::hand_over`] when the replacement was not published after all.
    pub fn roll_back(previous: Option<&Self>, candidate: &Self) {
        if let Err(error) = candidate.release() {
            log::error!("replacement stores could not be released: {error}");
        }
        restore(previous);
    }

    fn claim_in_order(&self) -> Result<(), StoreOwnershipError> {
        for disk in &self.disks {
            disk.claim().map_err(|source| match source {
                DiskCacheError::AlreadyOwned => StoreOwnershipError::DiskOwned {
                    store: disk.store_name().to_owned(),
                    root: disk.root().to_owned(),
                },
                source => StoreOwnershipError::Disk {
                    store: disk.store_name().to_owned(),
                    root: disk.root().to_owned(),
                    source,
                },
            })?;
        }
        for certificate in &self.acme {
            certificate
                .claim_state()
                .map_err(|source| StoreOwnershipError::Acme(Box::new(source)))?;
        }
        Ok(())
    }

    #[must_use]
    pub fn disk_roots(&self) -> Vec<PathBuf> {
        self.disks
            .iter()
            .map(|disk| disk.root().to_owned())
            .collect()
    }
}

fn restore(previous: Option<&ExclusiveStores>) {
    if let Some(Err(error)) = previous.map(ExclusiveStores::reclaim) {
        log::error!("previous stores could not be taken back: {error}");
    }
}
