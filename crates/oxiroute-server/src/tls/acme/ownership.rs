#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct LoadedSchedule {
    pub(super) disk_revision: String,
    pub(super) not_before_unix_seconds: Option<u64>,
    pub(super) not_after_unix_seconds: Option<u64>,
    pub(super) initial_issuance_due: bool,
}

impl ReconcileState {
    pub(super) fn load(
        revisions: &RevisionStore,
        certificate: &str,
        declared_dns_names: &[String],
        policy: &AcmeManagedPolicy,
        schedule: &LoadedSchedule,
        dns_provider_registered: bool,
    ) -> Self {
        let persisted = if schedule.initial_issuance_due {
            None
        } else {
            read_persisted_renewal(revisions, certificate).filter(|renewal| {
                renewal.certificate == certificate
                    && renewal.identifiers == declared_dns_names
                    && renewal.directory_url == policy.directory_url
                    && renewal.authenticator == challenge_string(policy.challenge)
                    && renewal.dns_provider
                        == policy.dns01.as_ref().map(|dns01| dns01.provider.clone())
                    && renewal.key_type == key_type_string(policy.key_type)
            })
        };
        let next_action_unix_seconds = persisted
            .as_ref()
            .and_then(|renewal| renewal.next_action_unix_seconds)
            .or_else(|| {
                if schedule.initial_issuance_due {
                    Some(0)
                } else {
                    schedule.not_before_unix_seconds.and_then(|not_before| {
                        schedule.not_after_unix_seconds.and_then(|not_after| {
                            stable_renewal_time(not_before, not_after, certificate)
                        })
                    })
                }
            });
        let auto_retry_blocked = persisted
            .as_ref()
            .is_some_and(|renewal| renewal.auto_retry_blocked);
        let paused = persisted.as_ref().is_some_and(|renewal| renewal.paused);
        let next_action_unix_seconds =
            next_action_unix_seconds.filter(|_| !auto_retry_blocked && !paused);
        let retry_attempt = persisted
            .as_ref()
            .map_or(0, |renewal| renewal.retry_attempt);
        let retry_at_unix_seconds = persisted
            .as_ref()
            .and_then(|renewal| renewal.retry_at_unix_seconds);
        let suggested_renewal_unix_seconds = persisted
            .as_ref()
            .and_then(|renewal| renewal.suggested_renewal_unix_seconds);
        let account_url = persisted
            .as_ref()
            .and_then(|renewal| renewal.account_url.clone());
        let renewal_info_url = persisted
            .as_ref()
            .and_then(|renewal| renewal.renewal_info_url.clone());
        let renewal_information_status = persisted
            .as_ref()
            .and_then(|renewal| {
                normalized_renewal_information_status(&renewal.renewal_information_status)
            })
            .unwrap_or(if renewal_info_url.is_some() {
                "pending"
            } else {
                "not_advertised"
            });
        let last_success_unix_seconds = persisted
            .as_ref()
            .and_then(|renewal| renewal.last_success_unix_seconds);
        let last_error_code = persisted
            .as_ref()
            .and_then(|renewal| renewal.last_error_code.as_deref())
            .filter(|code| is_known_error_code(code))
            .map(str::to_owned);
        let last_outcome = Some(AcmeManagedOutcome::Loaded.code());
        let dns_cleanup_status = if policy.challenge == AcmeChallengeType::Dns01 {
            persisted_dns_cleanup_status(revisions, certificate)
        } else {
            "not_applicable"
        };
        let dns_provider_health =
            dns_provider_health(policy, dns_provider_registered, dns_cleanup_status);
        Self {
            disk_revision: schedule.disk_revision.clone(),
            not_before_unix_seconds: schedule.not_before_unix_seconds,
            not_after_unix_seconds: schedule.not_after_unix_seconds,
            next_action_unix_seconds,
            retry_attempt,
            retry_at_unix_seconds,
            suggested_renewal_unix_seconds,
            auto_retry_blocked,
            account_url,
            renewal_info_url,
            job_status: paused.then_some(JobStatus::Paused),
            job_id: None,
            paused,
            last_success_unix_seconds,
            last_outcome,
            last_error_code,
            renewal_information_status,
            dns_provider_health,
            dns_cleanup_status,
        }
    }
}

fn dns_provider_health(
    policy: &AcmeManagedPolicy,
    dns_provider_registered: bool,
    dns_cleanup_status: &str,
) -> &'static str {
    if policy.challenge != AcmeChallengeType::Dns01 {
        "not_applicable"
    } else if !dns_provider_registered {
        "unsupported"
    } else if matches!(dns_cleanup_status, "pending" | "failed") {
        "degraded"
    } else {
        "unknown"
    }
}

impl AcmeManagedReconciler {
    #[must_use]
    pub fn certificate(&self) -> &str {
        &self.certificate
    }

    /// Takes the state root from the writer that owned it, then catches up with what that writer
    /// left: the selected revision, the renewal schedule and any unresolved DNS-01 cleanup. Claiming
    /// a state this reconciler already owns changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`TlsBuildError::AcmeStateOwned`] while the active worker still owns the root, and
    /// releases the root again when the state it finds cannot be loaded.
    pub fn claim_state(&self) -> Result<(), TlsBuildError> {
        self.take_state(false)
    }

    /// Takes the state root back after an aborted hand-over, waiting while the replacement that
    /// failed to activate still holds it.
    ///
    /// # Errors
    ///
    /// Returns an error when the root cannot be locked or the state cannot be loaded.
    pub fn reclaim_state(&self) -> Result<(), TlsBuildError> {
        self.take_state(true)
    }

    fn take_state(&self, wait: bool) -> Result<(), TlsBuildError> {
        let state = self.revisions.state();
        if state.is_owned() {
            return Ok(());
        }
        let taken = if wait { state.reclaim() } else { state.claim() };
        taken.map_err(|source| match source {
            AcmeStateError::StateLocked => TlsBuildError::AcmeStateOwned {
                certificate: self.certificate.clone(),
                state_root: state.root().to_owned(),
            },
            source => self.state_error(source),
        })?;
        self.catch_up().inspect_err(|_| state.release())?;
        self.notify_claimed();
        Ok(())
    }

    /// Stops this reconciler from writing its state root: a running job is cancelled and finished
    /// first, and renewal stays off until the root is claimed again. The certificate keeps serving.
    pub fn release_state(&self) {
        let _ = self.cancel_job();
        let _job = self
            .job
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.revisions.state().release();
    }

    /// Runs `listener` each time the state root is claimed, so renewal scheduling restarts.
    pub fn on_state_claimed(&self, listener: impl Fn() + Send + Sync + 'static) {
        self.claim_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Box::new(listener));
    }

    fn notify_claimed(&self) {
        for listener in &*self
            .claim_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            listener();
        }
    }

    fn catch_up(&self) -> Result<(), TlsBuildError> {
        let (generation, _, disk_revision, not_before, not_after, initial_issuance_due) =
            Self::load_from(
                self.revisions.clone(),
                &self.certificate,
                &self.declared_dns_names,
                self.policy.key_type,
            )?;
        if !initial_issuance_due {
            let expected = self.active.snapshot();
            if expected.metadata().revision != generation.metadata().revision {
                self.active
                    .publish_if_current(&expected, Arc::new(generation))
                    .map_err(|source| TlsBuildError::AcmePublication {
                        certificate: self.certificate.clone(),
                        source,
                    })?;
            }
        }
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = ReconcileState::load(
            &self.revisions,
            &self.certificate,
            &self.declared_dns_names,
            &self.policy,
            &LoadedSchedule {
                disk_revision,
                not_before_unix_seconds: not_before,
                not_after_unix_seconds: not_after,
                initial_issuance_due,
            },
            self.dns_provider.is_some(),
        );
        if self.dns_provider.is_some() {
            let _ = self.recover_pending_dns_cleanup();
        }
        Ok(())
    }
}
