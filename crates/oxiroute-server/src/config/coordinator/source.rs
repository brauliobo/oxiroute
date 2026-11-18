use oxiroute_config_source::{ConfigFormat, ConfigSourceError, resolve_source_with_format};

use super::{
    AuthoredRevision, CanonicalConfigCoordinator, CanonicalStorage, ConfigDraftRejection,
    ConfigLoadRejection, ConfigValidationOutcome, ConfigWriteBlock, EffectiveRevision,
    PersistableConfigCandidate, rejection, source_error_diagnostic,
};

/// The exact authored text of the canonical file and the revision of its bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredSource {
    pub text: String,
    pub revision: AuthoredRevision,
    pub format: ConfigFormat,
}

impl CanonicalConfigCoordinator {
    /// Reads the authored file without resolving it, so a file that no longer validates can still
    /// be shown and repaired.
    ///
    /// # Errors
    ///
    /// Returns the redacted rejection when the file cannot be read securely or is not UTF-8.
    pub fn read_source(&self) -> Result<AuthoredSource, ConfigLoadRejection> {
        let _operation = self.lock();
        let (bytes, revision) = self.read_disk()?;
        let text = String::from_utf8(bytes).map_err(|error| {
            rejection(
                Some(revision.clone()),
                source_error_diagnostic(&ConfigSourceError::Utf8(error.utf8_error())),
            )
        })?;
        Ok(AuthoredSource {
            text,
            revision,
            format: self.format,
        })
    }

    /// Resolves authored text exactly as the canonical file is resolved, without touching disk.
    ///
    /// Native references inside the text resolve against the directory of the canonical file.
    /// The candidate keeps the authored bytes, so saving it preserves comments, formatting and Lua
    /// logic.
    #[must_use]
    pub fn prepare_source(&self, text: &str, format: ConfigFormat) -> ConfigValidationOutcome {
        match resolve_source_with_format(&self.canonical_path, text.as_bytes(), format) {
            Ok(resolved) => ConfigValidationOutcome::Valid(Box::new(
                PersistableConfigCandidate::new(
                    EffectiveRevision::of(&resolved.canonical_kdl, &resolved.config),
                    resolved.config,
                    resolved.format,
                    resolved.compositional,
                    resolved.dependencies,
                    text.to_owned(),
                    Vec::new(),
                )
                .into_authored(),
            )),
            Err(error) => ConfigValidationOutcome::Invalid(ConfigDraftRejection {
                diagnostics: vec![source_error_diagnostic(&error)],
            }),
        }
    }

    /// The revision of the exact bytes on disk, without resolving them.
    ///
    /// # Errors
    ///
    /// Returns the redacted rejection when the file cannot be read securely.
    pub fn disk_revision(&self) -> Result<AuthoredRevision, ConfigLoadRejection> {
        let _operation = self.lock();
        self.read_disk().map(|(_, revision)| revision)
    }

    /// What stops a save, when something does: a read-only mount or a directory the process may
    /// not write.
    #[must_use]
    pub fn write_block(&self) -> Option<ConfigWriteBlock> {
        CanonicalStorage::open(&self.canonical_path)
            .map_or(Some(ConfigWriteBlock::Unavailable), |storage| {
                storage.write_block()
            })
    }
}
