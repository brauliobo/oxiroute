#[allow(clippy::wildcard_imports)]
use super::*;

/// Deduplicates recording-store preparation for one RTMP generation.
#[derive(Default)]
pub struct RtmpRecorderStoreRegistry {
    stores: HashMap<PathBuf, PreparedRecordingStore>,
}

struct PreparedRecordingStore {
    limits: RecordingStoreLimits,
    store: Option<RecordingStore>,
}

impl RtmpRecorderStoreRegistry {
    /// Preflights or opens one recording root according to the preparation mode.
    ///
    /// Validation scans each root once without opening a store. Activation reuses one opened store
    /// for every recorder that names the same root. Activation requires equal limits for every use
    /// of one root, matching the underlying process-wide recording store.
    ///
    /// # Errors
    ///
    /// Returns the underlying recording-store error when limits conflict or preparation fails.
    pub fn prepare(
        &mut self,
        root: impl AsRef<Path>,
        limits: RecordingStoreLimits,
        mode: RtmpPrepareMode,
        deadline: Option<Instant>,
    ) -> Result<Option<RecordingStore>, RecordingStoreError> {
        let root = root.as_ref();
        if let Some(prepared) = self.stores.get(root) {
            if mode == RtmpPrepareMode::Validation {
                return Ok(None);
            }
            if prepared.limits != limits {
                return Err(RecordingStoreError::LimitsMismatch);
            }
            if let Some(store) = &prepared.store {
                return Ok(Some(store.clone()));
            }
        }

        if mode == RtmpPrepareMode::Validation {
            RecordingStore::preflight(root, limits)?;
            self.stores.insert(
                root.to_owned(),
                PreparedRecordingStore {
                    limits,
                    store: None,
                },
            );
            return Ok(None);
        }

        let store = RecordingStore::open_with_deadline(root, limits, deadline)?;
        self.stores.insert(
            root.to_owned(),
            PreparedRecordingStore {
                limits,
                store: Some(store.clone()),
            },
        );
        Ok(Some(store))
    }
}
