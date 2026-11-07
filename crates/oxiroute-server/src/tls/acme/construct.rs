#[allow(clippy::wildcard_imports)]
use super::*;

impl AcmeManagedReconciler {
    /// Opens and validates the current managed revision before constructing the runtime slot.
    ///
    /// # Errors
    ///
    /// Returns an error when the managed current pointer, certificate material, or TLS validation
    /// cannot be read or accepted.
    #[allow(clippy::needless_pass_by_value)]
    pub(in super::super) fn load(
        certificate: impl Into<String>,
        declared_dns_names: &[String],
        state: Arc<StateStore>,
        key_type: AcmeKeyType,
    ) -> Result<LoadedManagedCertificate, TlsBuildError> {
        let certificate = certificate.into();
        let revisions = RevisionStore::from_arc(state);
        let material = match revisions.load_current(&certificate) {
            Ok(material) => material,
            Err(AcmeStateError::FileOpen(error)) if error.kind() == io::ErrorKind::NotFound => {
                let generation = CertificateGeneration::self_signed_development(
                    certificate,
                    declared_dns_names,
                    1,
                    bootstrap_key_type(key_type),
                )?;
                return Ok((generation, revisions, "bootstrap".into(), None, None, true));
            }
            Err(source) => {
                return Err(TlsBuildError::AcmeState {
                    certificate: certificate.clone(),
                    source: Box::new(source),
                });
            }
        };
        let current = revisions
            .state()
            .root()
            .join(format!("certificates/{certificate}/current"));
        let generation = CertificateGeneration::from_pem(
            certificate.clone(),
            declared_dns_names,
            &current.join("fullchain.pem"),
            &material.fullchain_pem,
            &current.join("privkey.pem"),
            material.private_key_pem.as_bytes(),
        )?;
        Ok((
            generation,
            revisions,
            material.metadata.revision,
            material.metadata.not_before_unix_seconds,
            material.metadata.not_after_unix_seconds,
            false,
        ))
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        certificate: impl Into<String>,
        declared_dns_names: Vec<String>,
        policy: AcmeManagedPolicy,
        revisions: RevisionStore,
        disk_revision: String,
        not_before_unix_seconds: Option<u64>,
        not_after_unix_seconds: Option<u64>,
        initial_issuance_due: bool,
        challenge_store: ChallengeStore,
        active: Arc<ActiveCertificateGeneration>,
    ) -> Self {
        Self::new_with_dns_provider(
            certificate,
            declared_dns_names,
            policy,
            revisions,
            disk_revision,
            not_before_unix_seconds,
            not_after_unix_seconds,
            initial_issuance_due,
            challenge_store,
            None,
            active,
        )
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub(in super::super) fn new_with_dns_provider(
        certificate: impl Into<String>,
        declared_dns_names: Vec<String>,
        policy: AcmeManagedPolicy,
        revisions: RevisionStore,
        disk_revision: String,
        not_before_unix_seconds: Option<u64>,
        not_after_unix_seconds: Option<u64>,
        initial_issuance_due: bool,
        challenge_store: ChallengeStore,
        dns_provider: Option<Arc<dyn Dns01Provider>>,
        active: Arc<ActiveCertificateGeneration>,
    ) -> Self {
        Self::new_with_challenge_stores(
            certificate,
            declared_dns_names,
            policy,
            revisions,
            disk_revision,
            not_before_unix_seconds,
            not_after_unix_seconds,
            initial_issuance_due,
            challenge_store,
            TlsAlpnChallengeStore::default(),
            dns_provider,
            active,
        )
    }

    #[must_use]
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(in super::super) fn new_with_challenge_stores(
        certificate: impl Into<String>,
        declared_dns_names: Vec<String>,
        policy: AcmeManagedPolicy,
        revisions: RevisionStore,
        disk_revision: String,
        not_before_unix_seconds: Option<u64>,
        not_after_unix_seconds: Option<u64>,
        initial_issuance_due: bool,
        challenge_store: ChallengeStore,
        tls_alpn_challenge_store: TlsAlpnChallengeStore,
        dns_provider: Option<Arc<dyn Dns01Provider>>,
        active: Arc<ActiveCertificateGeneration>,
    ) -> Self {
        let certificate = certificate.into();
        let persisted = if initial_issuance_due {
            None
        } else {
            read_persisted_renewal(&revisions, &certificate).filter(|renewal| {
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
                if initial_issuance_due {
                    Some(0)
                } else {
                    not_before_unix_seconds.and_then(|not_before| {
                        not_after_unix_seconds.and_then(|not_after| {
                            stable_renewal_time(not_before, not_after, &certificate)
                        })
                    })
                }
            });
        let auto_retry_blocked = persisted
            .as_ref()
            .is_some_and(|renewal| renewal.auto_retry_blocked);
        let next_action_unix_seconds = if auto_retry_blocked {
            None
        } else {
            next_action_unix_seconds
        };
        let paused = persisted.as_ref().is_some_and(|renewal| renewal.paused);
        let next_action_unix_seconds = if paused {
            None
        } else {
            next_action_unix_seconds
        };
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
            persisted_dns_cleanup_status(&revisions, &certificate)
        } else {
            "not_applicable"
        };
        let dns_provider_health = if policy.challenge != AcmeChallengeType::Dns01 {
            "not_applicable"
        } else if dns_provider.is_none() {
            "unsupported"
        } else if matches!(dns_cleanup_status, "pending" | "failed") {
            "degraded"
        } else {
            "unknown"
        };
        let reconciler = Self {
            certificate,
            declared_dns_names,
            policy,
            revisions,
            challenge_store,
            tls_alpn_challenge_store,
            dns_provider,
            active,
            job: Mutex::new(()),
            control: Mutex::new(JobControl::default()),
            state: Mutex::new(ReconcileState {
                disk_revision,
                not_before_unix_seconds,
                not_after_unix_seconds,
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
            }),
        };
        if reconciler.dns_provider.is_some() {
            let _ = reconciler.recover_pending_dns_cleanup();
        }
        reconciler
    }

    #[must_use]
    pub const fn active_generation(&self) -> &Arc<ActiveCertificateGeneration> {
        &self.active
    }

    #[must_use]
    pub const fn revisions(&self) -> &RevisionStore {
        &self.revisions
    }

    #[must_use]
    pub fn status(&self) -> AcmeManagedStatus {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let generation = self.active.snapshot();
        AcmeManagedStatus {
            certificate: self.certificate.clone(),
            directory_url: self.policy.directory_url.clone(),
            challenge: challenge_string(self.policy.challenge).into(),
            dns_provider: self
                .policy
                .dns01
                .as_ref()
                .map(|dns01| dns01.provider.clone()),
            key_type: match self.policy.key_type {
                AcmeKeyType::EcdsaP256 => "ecdsa_p256",
                AcmeKeyType::Rsa2048 => "rsa_2048",
            }
            .into(),
            allowed_dns_suffixes: self.policy.allowed_dns_suffixes.clone(),
            disk_revision: state.disk_revision.clone(),
            active_revision: generation.metadata().revision.clone(),
            not_before_unix_seconds: state.not_before_unix_seconds,
            not_after_unix_seconds: state.not_after_unix_seconds,
            next_action_unix_seconds: state.next_action_unix_seconds,
            not_after: generation.metadata().validity.not_after.clone(),
            job_status: state.job_status.clone(),
            job_id: state.job_id.clone(),
            paused: state.paused,
            retained_revisions: self.policy.retained_revisions,
            retention_days: self.policy.retention_days,
            retry_attempt: state.retry_attempt,
            last_success_unix_seconds: state.last_success_unix_seconds,
            last_outcome: state.last_outcome,
            last_error_code: state.last_error_code.clone(),
            renewal_information_status: state.renewal_information_status,
            dns_provider_deployment: (self.policy.challenge == AcmeChallengeType::Dns01).then_some(
                if self.dns_provider.is_some() {
                    "registered"
                } else {
                    "unsupported"
                },
            ),
            dns_provider_health: (self.policy.challenge == AcmeChallengeType::Dns01)
                .then_some(state.dns_provider_health),
            dns_cleanup_status: state.dns_cleanup_status,
        }
    }

    #[must_use]
    pub fn renewal_due(&self, now_unix_seconds: u64) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.paused || state.auto_retry_blocked {
            return false;
        }
        if let Some(next) = state.next_action_unix_seconds {
            return now_unix_seconds >= next;
        }
        match (state.not_before_unix_seconds, state.not_after_unix_seconds) {
            (Some(not_before), Some(not_after)) => renewal_due(
                now_unix_seconds,
                not_before,
                not_after,
                state.suggested_renewal_unix_seconds,
            ),
            _ => false,
        }
    }

    /// Reloads the atomically selected revision and publishes it only if it remains the same
    /// certificate identity and DNS set.
    ///
    /// # Errors
    ///
    /// Returns an error when the selected revision is unavailable, invalid, or loses a publication
    /// race with another writer.
    pub fn reconcile(&self) -> Result<AcmeManagedOutcome, TlsBuildError> {
        let _job = self
            .job
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let material = self
            .revisions
            .load_current(&self.certificate)
            .map_err(|source| self.state_error(source))?;
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
        .inspect_err(|_| {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.last_outcome = None;
            state.last_error_code = Some("invalid_candidate".into());
        })?;
        let expected = self.active.snapshot();
        let disk_revision = material.metadata.revision.clone();
        let outcome = if candidate.metadata().revision == expected.metadata().revision {
            AcmeManagedOutcome::Unchanged
        } else {
            self.active
                .publish_if_current(&expected, Arc::new(candidate))
                .map_err(|error| TlsBuildError::AcmePublication {
                    certificate: self.certificate.clone(),
                    source: error,
                })?;
            AcmeManagedOutcome::Activated
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.disk_revision = disk_revision;
        state.last_outcome = Some(outcome.code());
        state.last_error_code = None;
        Ok(outcome)
    }
}
