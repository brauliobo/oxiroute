use std::fs::File;

use crate::{
    RecordingEntry, RecordingEvictions, RecordingStatus, RecordingStore, RecordingStoreError,
    RecordingStoreLimits, RecordingStoreStats,
};

/// One configured recorder that writes into a recording root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecorderSource {
    pub service: String,
    pub application: String,
    pub recorder: String,
}

/// Quota use and retention counters of one recording root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingRootStatus {
    pub recorders: Vec<RecorderSource>,
    pub limits: RecordingStoreLimits,
    pub stats: RecordingStoreStats,
    pub evictions: RecordingEvictions,
}

/// A recording together with the recorders configured on its root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogRecording {
    pub entry: RecordingEntry,
    pub recorders: Vec<RecorderSource>,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordingCatalogError {
    #[error("recording does not exist")]
    NotFound,
    #[error("recording is being written")]
    Active,
    #[error("recording storage failed")]
    Store(#[source] RecordingStoreError),
}

struct RecordingRoot {
    store: RecordingStore,
    recorders: Vec<RecorderSource>,
}

/// Inventory of every recording root of one RTMP runtime set.
#[derive(Default)]
pub struct RecordingCatalog {
    roots: Vec<RecordingRoot>,
}

impl RecordingCatalog {
    pub(crate) fn from_recorders(
        recorders: impl IntoIterator<Item = (RecorderSource, RecordingStore)>,
    ) -> Self {
        let mut roots: Vec<RecordingRoot> = Vec::new();
        for (source, store) in recorders {
            match roots
                .iter_mut()
                .find(|root| root.store.is_same_store(&store))
            {
                Some(root) => root.recorders.push(source),
                None => roots.push(RecordingRoot {
                    store,
                    recorders: vec![source],
                }),
            }
        }
        Self { roots }
    }

    #[must_use]
    pub fn roots(&self) -> Vec<RecordingRootStatus> {
        self.roots
            .iter()
            .map(|root| RecordingRootStatus {
                recorders: root.recorders.clone(),
                limits: root.store.limits(),
                stats: root.store.stats(),
                evictions: root.store.evictions(),
            })
            .collect()
    }

    /// Lists the recordings of every root, newest first.
    ///
    /// # Errors
    ///
    /// Returns an error when a root cannot be enumerated.
    pub fn list(&self) -> Result<Vec<CatalogRecording>, RecordingCatalogError> {
        let mut recordings = Vec::new();
        for root in &self.roots {
            recordings.extend(
                root.store
                    .inventory()
                    .map_err(RecordingCatalogError::Store)?
                    .into_iter()
                    .map(|entry| CatalogRecording {
                        entry,
                        recorders: root.recorders.clone(),
                    }),
            );
        }
        recordings.sort_by(|left, right| {
            (right.entry.modified_unix_ms, &right.entry.id)
                .cmp(&(left.entry.modified_unix_ms, &left.entry.id))
        });
        Ok(recordings)
    }

    fn locate(
        &self,
        id: &str,
    ) -> Result<(&RecordingRoot, CatalogRecording), RecordingCatalogError> {
        for root in &self.roots {
            if let Some(entry) = root
                .store
                .inventory()
                .map_err(RecordingCatalogError::Store)?
                .into_iter()
                .find(|entry| entry.id == id)
            {
                let recording = CatalogRecording {
                    entry,
                    recorders: root.recorders.clone(),
                };
                return Ok((root, recording));
            }
        }
        Err(RecordingCatalogError::NotFound)
    }

    /// Returns the metadata of one recording.
    ///
    /// # Errors
    ///
    /// Returns an error when the recording does not exist or a root cannot be enumerated.
    pub fn get(&self, id: &str) -> Result<CatalogRecording, RecordingCatalogError> {
        self.locate(id).map(|(_, recording)| recording)
    }

    /// Opens a recording for reading and returns its length.
    ///
    /// # Errors
    ///
    /// Returns an error when the recording does not exist or cannot be opened.
    pub fn open(&self, id: &str) -> Result<(CatalogRecording, File, u64), RecordingCatalogError> {
        let (root, recording) = self.locate(id)?;
        let (file, length) = root
            .store
            .open_recording(&recording.entry.name)
            .map_err(RecordingCatalogError::Store)?;
        Ok((recording, file, length))
    }

    /// Deletes a recording that no recorder is writing.
    ///
    /// # Errors
    ///
    /// Returns an error when the recording does not exist, is being written, or cannot be removed.
    pub fn delete(&self, id: &str) -> Result<CatalogRecording, RecordingCatalogError> {
        let (root, recording) = self.locate(id)?;
        if recording.entry.status == RecordingStatus::Recording {
            return Err(RecordingCatalogError::Active);
        }
        root.store
            .delete_recording(&recording.entry.name)
            .map_err(|error| match error {
                RecordingStoreError::RecordingActive => RecordingCatalogError::Active,
                other => RecordingCatalogError::Store(other),
            })?;
        Ok(recording)
    }
}
