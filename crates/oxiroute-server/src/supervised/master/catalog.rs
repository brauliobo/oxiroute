#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug)]
pub(super) struct LaunchPayload {
    pub(super) config: ValidatedConfig,
    pub(super) identity: WorkerIdentity,
    pub(super) command: WorkerCommand,
}

pub(super) type LaunchDocument = GenerationLaunchDocument<EffectiveRevision, LaunchPayload>;
pub(super) type GenerationCatalog = SupervisedGenerationCatalog<EffectiveRevision, LaunchPayload>;

pub(super) fn worker_input(document: &LaunchDocument) -> WorkerInput<WorkerCommand> {
    WorkerInput {
        instance_id: document.instance_id().clone(),
        identity: document.payload().identity,
        command: document.payload().command.clone(),
    }
}

pub(super) fn has_restart_required_revision(
    catalog: &GenerationCatalog,
    revision: &EffectiveRevision,
) -> bool {
    catalog
        .restart_required()
        .is_some_and(|retained| retained.revision() == revision)
}

/// What the master has told the active worker about the configuration file.
///
/// A refused file is tried again on every reconciliation, so only a refusal that differs from the
/// reported one is logged and sent to the worker.
#[derive(Default)]
pub(super) struct ConfigReports {
    pub(super) rejection: Option<ConfigRejection>,
    /// The file the replacement in progress was started from.
    pub(super) candidate_file: Option<AuthoredRevision>,
    pub(super) retired: RetiredReports,
}

impl ConfigReports {
    pub(super) fn reject(&mut self, master: &mut Master, rejection: ConfigRejection) {
        if self.record(&rejection) {
            rejection.log();
            send_notice(
                master,
                &WorkerNotice::Config {
                    rejection: Some(rejection),
                },
            );
        }
    }

    pub(super) fn accept(&mut self, master: &mut Master) {
        if self.forget() {
            send_notice(master, &WorkerNotice::Config { rejection: None });
        }
    }

    /// The candidate was refused by its own worker, which logged why; the old worker keeps serving.
    pub(super) fn replacement_failed(&mut self, master: &mut Master, detail: String) {
        if let Some(file) = self.candidate_file.take() {
            let message = "the configuration could not be activated by a replacement worker";
            self.reject(master, refusal(&file, "E_RUNTIME_PREPARE", message, detail));
        }
    }

    /// Remembers `rejection` and reports whether it differs from the one already reported.
    pub(super) fn record(&mut self, rejection: &ConfigRejection) -> bool {
        let reported = self.rejection.as_ref().is_some_and(|reported| {
            reported.revision == rejection.revision && reported.diagnostics == rejection.diagnostics
        });
        if !reported {
            self.rejection = Some(rejection.clone());
        }
        !reported
    }

    pub(super) fn forget(&mut self) -> bool {
        self.rejection.take().is_some()
    }
}

pub(super) fn send_notice(master: &mut Master, notice: &WorkerNotice) {
    let notice = serde_json::to_vec(notice).expect("a worker notice serializes");
    if let Err(error) = master.notify_active(&notice) {
        warn!("supervised master could not notify the active worker: {error}");
    }
}

pub(super) fn refusal(
    file: &AuthoredRevision,
    code: &'static str,
    message: &'static str,
    detail: String,
) -> ConfigRejection {
    ConfigRejection::new(
        Some(file),
        &[ConfigDiagnostic {
            code,
            severity: ConfigDiagnosticSeverity::Error,
            stage: ConfigDiagnosticStage::Validation,
            message,
            path: None,
            detail: Some(detail),
            line: None,
        }],
    )
}

/// The listener reservations behind the master's stable listener set.
///
/// `active` mirrors the sockets the committed workers serve, so the next reload reuses the ones
/// it keeps and binds only the ones it adds. A candidate's reservations wait in `pending` until
/// its replacement commits and are dropped, closing the sockets they added, when it does not.
pub(super) struct ListenerBook {
    pub(super) active: ListenerReservations,
    pub(super) active_manifest: DescriptorManifest,
    pub(super) pending: Option<(ListenerReservations, DescriptorManifest)>,
}

impl ListenerBook {
    pub(super) const fn new(
        active: ListenerReservations,
        active_manifest: DescriptorManifest,
    ) -> Self {
        Self {
            active,
            active_manifest,
            pending: None,
        }
    }

    pub(super) fn begin(
        &mut self,
        reservations: ListenerReservations,
        manifest: DescriptorManifest,
        changes: &ListenerChanges,
        revision: &EffectiveRevision,
    ) {
        self.pending = Some((reservations, manifest));
        info!(
            "supervised master starts configuration revision {revision}: listeners added {:?}, \
             removed {:?}, kept {}",
            changes.added,
            changes.removed,
            changes.kept.len()
        );
    }

    fn commit(&mut self) {
        if let Some((reservations, manifest)) = self.pending.take() {
            self.active = reservations;
            self.active_manifest = manifest;
        }
    }

    pub(super) fn discard(&mut self) {
        self.pending = None;
    }
}

/// Applies the master's events to the catalog and says why a candidate failed, if one did.
pub(super) fn apply_catalog_events(
    catalog: &mut GenerationCatalog,
    events: &[MasterEvent],
    book: &mut ListenerBook,
) -> Result<Option<String>, CatalogSyncError> {
    let mut failure = None;
    for event in events {
        match event {
            MasterEvent::SpawnFailed { instance_id } => {
                expect_catalog_instance(catalog, GenerationRole::Candidate, instance_id)?;
                catalog.quarantine_candidate()?;
                book.discard();
                failure = Some("the replacement worker could not be started".to_owned());
            }
            MasterEvent::ReplacementCommitted { active, retired } => {
                expect_catalog_instance(catalog, GenerationRole::Candidate, active)?;
                expect_catalog_instance(catalog, GenerationRole::Active, retired)?;
                catalog.commit_candidate()?;
                book.commit();
            }
            MasterEvent::ReplacementCompleted { active }
            | MasterEvent::RollbackCompleted { active } => {
                expect_catalog_instance(catalog, GenerationRole::Active, active)?;
            }
            MasterEvent::RollbackStarted { candidate, phase } => {
                expect_catalog_instance(catalog, GenerationRole::Candidate, candidate)?;
                catalog.quarantine_candidate()?;
                book.discard();
                failure = Some(format!("the replacement worker failed during {phase:?}"));
            }
            _ => {}
        }
    }
    Ok(failure)
}

fn expect_catalog_instance(
    catalog: &GenerationCatalog,
    role: GenerationRole,
    actual: &InstanceId,
) -> Result<(), CatalogSyncError> {
    let expected = catalog.get(role).map(GenerationLaunchDocument::instance_id);
    if expected == Some(actual) {
        return Ok(());
    }
    Err(CatalogSyncError::UnexpectedInstance {
        role,
        expected: expected.cloned(),
        actual: actual.clone(),
    })
}

#[derive(Debug, Error)]
pub(super) enum CatalogSyncError {
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(
        "master event referenced generation instance {actual} as {role:?}, expected {expected:?}"
    )]
    UnexpectedInstance {
        role: GenerationRole,
        expected: Option<InstanceId>,
        actual: InstanceId,
    },
}
