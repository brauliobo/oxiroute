#[allow(clippy::wildcard_imports)]
use super::*;

impl AcmeManagedReconciler {
    pub(super) fn certificate_material(
        &self,
        csr: &oxiroute_acme::LeafCsr,
        certificate_pem: &[u8],
    ) -> Result<(CertificateMaterial, CertificateGeneration), AcmeManagedError> {
        if certificate_pem.len() > MAX_CERTIFICATE_CHAIN_BYTES {
            return Err(AcmeManagedError::CertificateMalformed);
        }
        let certificates = X509::stack_from_pem(certificate_pem)
            .map_err(|_| AcmeManagedError::CertificateMalformed)?;
        if certificates.len() < 2 {
            return Err(AcmeManagedError::CertificateMalformed);
        }
        validate_exact_dns_sans(&certificates[0], &self.declared_dns_names)?;
        let mut fullchain_pem = Vec::new();
        let mut chain_pem = Vec::new();
        for (index, certificate) in certificates.iter().enumerate() {
            let pem = certificate
                .to_pem()
                .map_err(|_| AcmeManagedError::CertificateMalformed)?;
            fullchain_pem.extend_from_slice(&pem);
            if index > 0 {
                chain_pem.extend_from_slice(&pem);
            }
        }
        let certificate_pem = certificates[0]
            .to_pem()
            .map_err(|_| AcmeManagedError::CertificateMalformed)?;
        let serial = certificates[0]
            .serial_number()
            .to_bn()
            .map_err(|_| AcmeManagedError::CertificateMalformed)?;
        let mut material = CertificateMaterial {
            certificate_pem,
            chain_pem,
            fullchain_pem,
            private_key_pem: csr.private_key_pem.clone(),
            metadata: RevisionMetadata {
                certificate: self.certificate.clone(),
                revision: "pending".into(),
                created_at_unix_seconds: unix_now(),
                not_before_unix_seconds: asn1_unix(certificates[0].not_before()),
                not_after_unix_seconds: asn1_unix(certificates[0].not_after()),
                issuer: Some(issuer_name(&certificates[0])?),
                serial_fingerprint: Some(crate::encoding::lower_hex(&openssl::sha::sha256(
                    &serial.to_vec(),
                ))),
                key_type: Some(
                    match self.policy.key_type {
                        AcmeKeyType::EcdsaP256 => "ecdsa_p256",
                        AcmeKeyType::Rsa2048 => "rsa_2048",
                    }
                    .into(),
                ),
            },
        };
        let root = self.revisions.state().root();
        let current = root.join(format!("certificates/{}/current", self.certificate));
        let candidate = CertificateGeneration::from_pem(
            self.certificate.clone(),
            &self.declared_dns_names,
            &current.join("fullchain.pem"),
            &material.fullchain_pem,
            &current.join("privkey.pem"),
            material.private_key_pem.as_bytes(),
        )
        .map_err(|source| AcmeManagedError::Tls(Box::new(source)))?;
        material
            .metadata
            .revision
            .clone_from(&candidate.metadata().revision);
        Ok((material, candidate))
    }

    pub(super) fn try_lock_job(&self) -> Result<std::sync::MutexGuard<'_, ()>, AcmeManagedError> {
        match self.job.try_lock() {
            Ok(job) => Ok(job),
            Err(TryLockError::Poisoned(poisoned)) => Ok(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => Err(AcmeManagedError::Busy),
        }
    }

    pub(super) fn begin_action_job(
        &self,
        operation: &str,
        correlation_id: &str,
    ) -> Result<String, AcmeManagedError> {
        let now = unix_now();
        let sequence = NEXT_JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let job_id = format!("{operation}-{now}-{sequence}");
        self.set_job_id(Some(job_id.clone()));
        self.set_job_status(Some(JobStatus::Queued));
        if let Err(error) = self.revisions.state().create_job(
            &job_id,
            &self.certificate,
            operation,
            Some(correlation_id.into()),
            now,
        ) {
            self.set_job_id(None);
            self.set_job_status(None);
            return Err(AcmeManagedError::State(error));
        }
        if let Err(error) = self.revisions.state().start_job(&job_id, unix_now()) {
            self.set_job_id(None);
            self.set_job_status(None);
            return Err(AcmeManagedError::State(error));
        }
        self.set_job_status(Some(JobStatus::Running));
        Ok(job_id)
    }

    pub(super) fn finish_action_job(
        &self,
        job_id: &str,
        result: &Result<(), AcmeManagedError>,
        success: AcmeManagedOutcome,
    ) -> Result<(), AcmeManagedError> {
        let status = self.status();
        let (job_status, outcome_code, outcome, error_code) = match result {
            Ok(()) => (
                JobStatus::Succeeded,
                success.code(),
                Some(RedactedOutcome::new(
                    success.code(),
                    "managed ACME action completed",
                )),
                None,
            ),
            Err(error) => (
                JobStatus::Failed,
                error.code(),
                Some(RedactedOutcome::new(
                    error.code(),
                    "managed ACME action failed",
                )),
                Some(error.code()),
            ),
        };
        self.set_outcome(Some(outcome_code), error_code);
        self.set_job_status(Some(job_status.clone()));
        let outcome = outcome.expect("action jobs always emit a terminal outcome");
        match job_status {
            JobStatus::Succeeded => self.revisions.state().succeed_job(
                job_id,
                unix_now(),
                Some(status.disk_revision),
                Some(status.active_revision),
                outcome,
            ),
            JobStatus::Failed => self.revisions.state().fail_job(
                job_id,
                unix_now(),
                None,
                Some(status.disk_revision),
                Some(status.active_revision),
                outcome,
            ),
            _ => unreachable!("administrative actions have terminal job states"),
        }
        .map(|_| ())
        .map_err(AcmeManagedError::State)
    }

    pub(super) fn set_outcome(&self, outcome: Option<&'static str>, error: Option<&'static str>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.last_outcome = outcome;
        state.last_error_code = error.map(str::to_owned);
    }

    pub(super) fn set_job_status(&self, status: Option<JobStatus>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .job_status = status;
    }

    pub(super) fn set_job_id(&self, id: Option<String>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .job_id = id;
    }

    pub(super) fn is_paused(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .paused
    }

    pub(super) fn clear_job_control(&self, id: &str) {
        let mut control = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if control.id.as_deref() == Some(id) {
            control.id = None;
            control.cancellation = None;
            self.set_job_id(None);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn update_schedule(
        &self,
        not_before_unix_seconds: Option<u64>,
        not_after_unix_seconds: Option<u64>,
        disk_revision: String,
        outcome: &'static str,
        account_url: Option<String>,
        renewal_info_url: Option<String>,
        renewal_information: Option<&RenewalInformation>,
        renewal_information_status: &'static str,
    ) -> Result<(), AcmeStateError> {
        let local_next_action_unix_seconds = not_before_unix_seconds.and_then(|not_before| {
            not_after_unix_seconds
                .and_then(|not_after| stable_renewal_time(not_before, not_after, &self.certificate))
        });
        let suggested_renewal_unix_seconds = renewal_information
            .zip(not_before_unix_seconds.zip(not_after_unix_seconds))
            .and_then(|(information, (not_before, not_after))| {
                stable_renewal_time_in_window(
                    information.suggested_window_start_unix_seconds,
                    information.suggested_window_end_unix_seconds,
                    not_before,
                    not_after,
                    &self.certificate,
                )
            });
        let next_action_unix_seconds =
            suggested_renewal_unix_seconds.or(local_next_action_unix_seconds);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.disk_revision = disk_revision;
        state.not_before_unix_seconds = not_before_unix_seconds;
        state.not_after_unix_seconds = not_after_unix_seconds;
        state.next_action_unix_seconds = next_action_unix_seconds;
        state.retry_attempt = 0;
        state.retry_at_unix_seconds = None;
        state.suggested_renewal_unix_seconds = suggested_renewal_unix_seconds;
        state.auto_retry_blocked = false;
        state.account_url = account_url.or_else(|| state.account_url.clone());
        state.renewal_info_url = renewal_info_url.or_else(|| state.renewal_info_url.clone());
        state.renewal_information_status = renewal_information_status;
        state.last_success_unix_seconds = Some(unix_now());
        state.last_outcome = Some(outcome);
        state.last_error_code = None;
        drop(state);
        self.persist_renewal()?;
        self.revisions.garbage_collect(
            &self.certificate,
            self.policy.retained_revisions as usize,
            unix_now().saturating_sub(u64::from(self.policy.retention_days) * 86_400),
        )?;
        Ok(())
    }

    pub(super) fn schedule_retry(&self, now_unix_seconds: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let attempt = state.retry_attempt.saturating_add(1);
        let delay = retry_delay(&self.certificate, attempt);
        state.retry_attempt = attempt;
        state.retry_at_unix_seconds = Some(now_unix_seconds.saturating_add(delay));
        state.next_action_unix_seconds = state.retry_at_unix_seconds;
        state.auto_retry_blocked = false;
    }

    pub(super) fn block_automatic_retry(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.retry_at_unix_seconds = None;
        state.next_action_unix_seconds = None;
        state.auto_retry_blocked = true;
    }

    pub(super) fn persist_renewal(&self) -> Result<(), AcmeStateError> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let renewal = PersistedRenewal {
            certificate: self.certificate.clone(),
            identifiers: self.declared_dns_names.clone(),
            directory_url: self.policy.directory_url.clone(),
            account_url: state.account_url.clone(),
            authenticator: challenge_string(self.policy.challenge).into(),
            dns_provider: self
                .policy
                .dns01
                .as_ref()
                .map(|dns01| dns01.provider.clone()),
            key_type: key_type_string(self.policy.key_type).into(),
            next_action_unix_seconds: state.next_action_unix_seconds,
            retry_at_unix_seconds: state.retry_at_unix_seconds,
            retry_attempt: state.retry_attempt,
            suggested_renewal_unix_seconds: state.suggested_renewal_unix_seconds,
            auto_retry_blocked: state.auto_retry_blocked,
            renewal_info_url: state.renewal_info_url.clone(),
            last_success_unix_seconds: state.last_success_unix_seconds,
            last_error_code: state.last_error_code.clone(),
            renewal_information_status: state.renewal_information_status.into(),
            paused: state.paused,
        };
        drop(state);
        self.revisions.state().write_json(
            &format!("certificates/{}/renewal.json", self.certificate),
            &renewal,
        )
    }

    pub(super) fn state_error(&self, source: AcmeStateError) -> TlsBuildError {
        TlsBuildError::AcmeState {
            certificate: self.certificate.clone(),
            source: Box::new(source),
        }
    }
}
