use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

use openssl::sha::sha256;

use super::model::{Overlay, OverlayEdit};
use crate::{atomic_file::write_atomic, encoding::lower_hex};

const FILE_MODE: u32 = 0o600;
const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub(crate) enum OverlayError {
    #[error("the overlay file is not accessible")]
    Io(#[from] io::Error),
    #[error("the overlay file is not a valid overlay")]
    Invalid,
    #[error("the overlay file has an unknown schema version")]
    UnknownSchema,
    #[error("the overlay file is larger than {MAX_BYTES} bytes or lists too many entries")]
    TooLarge,
}

#[derive(Clone, Debug)]
pub(crate) struct OverlayStore {
    path: PathBuf,
    writers: Arc<Mutex<()>>,
}

impl OverlayStore {
    pub(crate) fn beside(canonical_path: &Path) -> Self {
        let mut path = canonical_path.as_os_str().to_owned();
        path.push(".overlay.json");
        Self {
            path: PathBuf::from(path),
            writers: Arc::default(),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn load(&self) -> Result<Overlay, OverlayError> {
        Ok(self
            .read()?
            .map_or_else(Overlay::default, |(overlay, _)| overlay))
    }

    pub(crate) fn revision(&self) -> Result<Option<String>, OverlayError> {
        Ok(self.read()?.map(|(_, revision)| revision))
    }

    pub(crate) fn edit(
        &self,
        edits: impl IntoIterator<Item = OverlayEdit>,
    ) -> Result<Overlay, OverlayError> {
        self.update(|overlay| overlay.edited(edits))
    }

    pub(crate) fn update(
        &self,
        change: impl FnOnce(Overlay) -> Overlay,
    ) -> Result<Overlay, OverlayError> {
        let _writer = self.writers.lock().unwrap_or_else(PoisonError::into_inner);
        let overlay = change(self.load()?);
        self.write(&overlay)?;
        Ok(overlay)
    }

    pub(crate) fn clear(&self) -> io::Result<bool> {
        let _writer = self.writers.lock().unwrap_or_else(PoisonError::into_inner);
        self.remove()
    }

    fn remove(&self) -> io::Result<bool> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn write(&self, overlay: &Overlay) -> Result<(), OverlayError> {
        if overlay.is_empty() {
            return Ok(self.remove().map(drop)?);
        }
        let mut text = serde_json::to_string_pretty(overlay).map_err(|_| OverlayError::Invalid)?;
        text.push('\n');
        Ok(write_atomic(&self.path, text.as_bytes(), FILE_MODE)?)
    }

    fn read(&self) -> Result<Option<(Overlay, String)>, OverlayError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if bytes.len() as u64 > MAX_BYTES {
            return Err(OverlayError::TooLarge);
        }
        let mut overlay: Overlay =
            serde_json::from_slice(&bytes).map_err(|_| OverlayError::Invalid)?;
        if !overlay.has_known_schema() {
            return Err(OverlayError::UnknownSchema);
        }
        if !overlay.within_bounds() {
            return Err(OverlayError::TooLarge);
        }
        overlay.normalize();
        Ok(Some((overlay, lower_hex(&sha256(&bytes)))))
    }
}
