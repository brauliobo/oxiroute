use super::{
    ConfigLoadRejection, ConfigValidationOutcome, ResolvedConfigDocument, prepare_candidate,
};
use crate::generation::restart::StartupOnly;

impl ResolvedConfigDocument {
    /// The document a process applies: this one with every `kept` setting as the process has it.
    ///
    /// Its effective revision is the one the process reports as active, while the authored revision
    /// stays the one of the file.
    ///
    /// # Errors
    ///
    /// Returns the rejection when the configuration with the kept settings is not valid.
    pub fn keeping(&self, kept: &[StartupOnly]) -> Result<Self, ConfigLoadRejection> {
        if kept.is_empty() {
            return Ok(self.clone());
        }
        let mut draft = self.validated_config.to_draft();
        for setting in kept {
            setting.keep_running(&mut draft);
        }
        match prepare_candidate(self.format, draft) {
            ConfigValidationOutcome::Valid(candidate) => Ok(Self {
                authored_revision: self.authored_revision.clone(),
                effective_revision: candidate.effective_revision().clone(),
                validated_config: candidate.validated_config().clone(),
                format: self.format,
                compositional: self.compositional,
                dependencies: self.dependencies.clone(),
                config_preview: candidate.config_preview().to_owned(),
                diagnostics: self.diagnostics.clone(),
            }),
            ConfigValidationOutcome::Invalid(rejection) => Err(ConfigLoadRejection {
                disk_revision: Some(self.authored_revision.clone()),
                diagnostics: rejection.diagnostics,
            }),
        }
    }
}
