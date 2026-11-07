#[allow(clippy::wildcard_imports)]
use super::*;

impl AcmeManagedReconciler {
    #[allow(clippy::too_many_lines)]
    pub(super) fn renew_with_provider<T: AcmeTransport>(
        &self,
        transport: T,
        provider: Option<Arc<dyn Dns01Provider>>,
        correlation_id: &str,
        operation: AcmeOperation,
    ) -> Result<AcmeManagedOutcome, AcmeManagedError> {
        let _job = match self.job.try_lock() {
            Ok(job) => job,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return Err(AcmeManagedError::Busy),
        };
        if self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .paused
        {
            return Err(AcmeManagedError::Paused);
        }
        let now = unix_now();
        let sequence = NEXT_JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let job_id = format!("renew-{now}-{sequence}");
        operation.check().map_err(AcmeManagedError::Protocol)?;
        let cancellation = operation.cancellation().clone();
        {
            let mut control = self
                .control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            control.id = Some(job_id.clone());
            control.cancellation = Some(cancellation.clone());
        }
        self.set_job_id(Some(job_id.clone()));
        self.set_job_status(Some(JobStatus::Queued));
        if let Err(error) = self.revisions.state().create_job(
            &job_id,
            &self.certificate,
            "renew",
            Some(correlation_id.to_owned()),
            now,
        ) {
            self.clear_job_control(&job_id);
            return Err(AcmeManagedError::State(error));
        }
        if let Err(error) = self.revisions.state().start_job(&job_id, unix_now()) {
            self.clear_job_control(&job_id);
            return Err(AcmeManagedError::State(error));
        }
        self.set_job_status(Some(JobStatus::Running));
        let result = self.renew_inner(transport, &job_id, now, provider, operation);
        match &result {
            Ok(outcome) => {
                let status = self.status();
                self.set_outcome(Some(outcome.code()), None);
                self.set_job_status(Some(JobStatus::Succeeded));
                let write_result = self.revisions.state().succeed_job(
                    &job_id,
                    unix_now(),
                    Some(status.disk_revision.clone()),
                    Some(status.active_revision.clone()),
                    RedactedOutcome::new(outcome.code(), "managed ACME renewal completed"),
                );
                if let Err(error) = write_result {
                    self.clear_job_control(&job_id);
                    return Err(AcmeManagedError::State(error));
                }
            }
            Err(error) => {
                let paused = self.is_paused();
                let cancelled = is_cancelled_error(error);
                if !paused && !cancelled {
                    if error.is_retryable() {
                        self.schedule_retry(unix_now());
                    } else {
                        self.block_automatic_retry();
                    }
                }
                self.set_outcome(
                    if paused {
                        Some("paused")
                    } else if cancelled {
                        Some("cancelled")
                    } else {
                        None
                    },
                    if paused || cancelled {
                        None
                    } else {
                        Some(error.code())
                    },
                );
                let renewal_error = if paused || cancelled {
                    None
                } else {
                    self.persist_renewal().err()
                };
                if renewal_error.is_some() {
                    self.set_outcome(None, Some("state_failed"));
                }
                let job_status = if paused {
                    JobStatus::Paused
                } else if cancelled {
                    JobStatus::Cancelled
                } else {
                    JobStatus::Failed
                };
                self.set_job_status(Some(job_status.clone()));
                let status = self.status();
                let outcome_code = if renewal_error.is_some() {
                    "state_failed"
                } else if paused {
                    "paused"
                } else if cancelled {
                    "cancelled"
                } else {
                    error.code()
                };
                let job_error = match job_status {
                    JobStatus::Paused => self.revisions.state().pause_job(
                        &job_id,
                        unix_now(),
                        Some(status.disk_revision),
                        Some(status.active_revision),
                        RedactedOutcome::new(outcome_code, "managed ACME renewal failed"),
                    ),
                    JobStatus::Cancelled => self.revisions.state().cancel_job(
                        &job_id,
                        unix_now(),
                        Some(status.disk_revision),
                        Some(status.active_revision),
                        RedactedOutcome::new(outcome_code, "managed ACME renewal failed"),
                    ),
                    JobStatus::Failed => self.revisions.state().fail_job(
                        &job_id,
                        unix_now(),
                        status.next_action_unix_seconds,
                        Some(status.disk_revision),
                        Some(status.active_revision),
                        RedactedOutcome::new(outcome_code, "managed ACME renewal failed"),
                    ),
                    _ => unreachable!("renewal errors have terminal job states"),
                }
                .err();
                if let Some(state_error) = renewal_error.or(job_error) {
                    self.clear_job_control(&job_id);
                    return Err(AcmeManagedError::State(state_error));
                }
            }
        }
        self.clear_job_control(&job_id);
        result
    }

    #[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
    fn renew_inner<T: AcmeTransport>(
        &self,
        transport: T,
        job_id: &str,
        created_at: u64,
        dns_provider: Option<Arc<dyn Dns01Provider>>,
        operation: AcmeOperation,
    ) -> Result<AcmeManagedOutcome, AcmeManagedError> {
        if self.policy.challenge == AcmeChallengeType::Dns01 {
            let recovery = self.recover_pending_dns_cleanup_with_operation(&operation)?;
            if recovery == AcmeDnsCleanupRecovery::Deferred {
                return Err(AcmeManagedError::DnsCleanup(
                    Dns01ProviderError::CleanupFailed,
                ));
            }
        }
        operation.check().map_err(AcmeManagedError::Protocol)?;
        let cancellation = operation.cancellation().clone();
        let mut client = self.client_with_account(transport, true, operation.clone())?;
        let dns_context = match self.policy.challenge {
            AcmeChallengeType::Dns01 => {
                let dns01 = self
                    .policy
                    .dns01
                    .as_ref()
                    .ok_or(AcmeManagedError::DnsProviderUnsupported)?;
                let provider = dns_provider
                    .as_ref()
                    .filter(|provider| provider.name() == dns01.provider)
                    .ok_or(AcmeManagedError::DnsProviderUnsupported)?;
                let credentials = load_dns_credentials(&dns01.credential_file, &self.certificate)?;
                Some((Arc::clone(provider), credentials, dns01.timeout_seconds))
            }
            AcmeChallengeType::Http01 | AcmeChallengeType::TlsAlpn01 => None,
        };
        let order = client
            .create_order(&oxiroute_acme::CertificateRequest {
                identifiers: self.declared_dns_names.clone(),
            })
            .map_err(AcmeManagedError::Protocol)?;
        let declared_identifiers = self
            .declared_dns_names
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        let mut authorized_identifiers = BTreeSet::new();
        if !order.authorizations.is_empty() {
            self.set_job_status(Some(JobStatus::WaitingForChallenge));
            self.revisions
                .state()
                .wait_job(job_id, unix_now())
                .map_err(AcmeManagedError::State)?;
        }
        for (index, authorization_url) in order.authorizations.iter().enumerate() {
            let authorization = match self.policy.challenge {
                AcmeChallengeType::Http01 => client.authorization(authorization_url),
                AcmeChallengeType::Dns01 => {
                    client.authorization_for(authorization_url, ChallengeType::Dns01)
                }
                AcmeChallengeType::TlsAlpn01 => {
                    client.authorization_for(authorization_url, ChallengeType::TlsAlpn01)
                }
            }
            .map_err(AcmeManagedError::Protocol)?;
            if !declared_identifiers.contains(&authorization.identifier)
                || !authorized_identifiers.insert(authorization.identifier.clone())
            {
                return Err(AcmeManagedError::AuthorizationFailed);
            }
            if authorization.status == AuthorizationStatus::Valid {
                continue;
            }
            if !matches!(
                authorization.status,
                AuthorizationStatus::Pending | AuthorizationStatus::Processing
            ) {
                return Err(AcmeManagedError::AuthorizationFailed);
            }
            if self.policy.challenge == AcmeChallengeType::Dns01 {
                let (provider, credentials, timeout_seconds) = dns_context
                    .as_ref()
                    .ok_or(AcmeManagedError::DnsProviderUnsupported)?;
                let challenge = authorization
                    .dns01_challenge
                    .as_ref()
                    .ok_or(AcmeManagedError::Protocol(AcmeError::UnsupportedChallenge))?;
                self.complete_dns01_challenge(
                    &mut client,
                    &authorization.url,
                    challenge,
                    provider.as_ref(),
                    credentials,
                    *timeout_seconds,
                    &operation,
                )?;
                continue;
            }
            if self.policy.challenge == AcmeChallengeType::TlsAlpn01 {
                let challenge = authorization
                    .tls_alpn01_challenge
                    .as_ref()
                    .ok_or(AcmeManagedError::Protocol(AcmeError::UnsupportedChallenge))?;
                let created_at = unix_now();
                let identity = TlsAlpnChallenge::generate(
                    &authorization.identifier,
                    &challenge.key_authorization,
                    "managed-account",
                    job_id,
                    &format!("authorization-{index}"),
                    &format!("challenge-{index}"),
                    created_at,
                    created_at.saturating_add(600),
                )
                .map_err(|error| AcmeManagedError::TlsAlpnChallenge(Box::new(error)))?;
                let lease = self
                    .tls_alpn_challenge_store
                    .provision(identity)
                    .map_err(|error| AcmeManagedError::TlsAlpnChallenge(Box::new(error)))?;
                client
                    .respond_to_tls_alpn01_challenge(challenge)
                    .map_err(AcmeManagedError::Protocol)?;
                let authorization = client
                    .poll_authorization_for(
                        &authorization.url,
                        &poll_policy(unix_now().saturating_add(600), Some(cancellation.clone())),
                        ChallengeType::TlsAlpn01,
                    )
                    .map_err(AcmeManagedError::Protocol)?;
                lease.complete();
                if authorization.status != AuthorizationStatus::Valid {
                    return Err(AcmeManagedError::AuthorizationFailed);
                }
                continue;
            }
            let challenge = authorization
                .challenge
                .ok_or(AcmeManagedError::Protocol(AcmeError::UnsupportedChallenge))?;
            let lease = self
                .challenge_store
                .provision(ChallengeRecord {
                    token: challenge.token.clone(),
                    key_authorization: challenge.key_authorization.clone(),
                    account_id: "managed-account".into(),
                    order_id: job_id.into(),
                    authorization_id: format!("authorization-{index}"),
                    challenge_id: format!("challenge-{index}"),
                    created_at_unix_seconds: created_at,
                    expires_at_unix_seconds: created_at.saturating_add(600),
                })
                .map_err(AcmeManagedError::Challenge)?;
            client
                .respond_to_challenge(&challenge)
                .map_err(AcmeManagedError::Protocol)?;
            let authorization = client
                .poll_authorization(
                    &authorization.url,
                    &poll_policy(unix_now().saturating_add(600), Some(cancellation.clone())),
                )
                .map_err(AcmeManagedError::Protocol)?;
            lease.complete();
            if authorization.status != AuthorizationStatus::Valid {
                return Err(AcmeManagedError::AuthorizationFailed);
            }
        }
        if authorized_identifiers != declared_identifiers {
            return Err(AcmeManagedError::AuthorizationFailed);
        }
        self.set_job_status(Some(JobStatus::Finalizing));
        self.revisions
            .state()
            .finalize_job(job_id, unix_now())
            .map_err(AcmeManagedError::State)?;
        let csr = oxiroute_acme::generate_leaf_csr(
            &self.declared_dns_names,
            leaf_key_algorithm(self.policy.key_type),
        )
        .map_err(AcmeManagedError::Protocol)?;
        let finalized = client
            .finalize_order(&order, &csr.csr_der)
            .map_err(AcmeManagedError::Protocol)?;
        let valid_order = client
            .poll_order(
                &finalized.url,
                &poll_policy(unix_now().saturating_add(600), Some(cancellation)),
            )
            .map_err(AcmeManagedError::Protocol)?;
        if valid_order.status != "valid" {
            return Err(AcmeManagedError::OrderFailed);
        }
        let certificate_url = valid_order
            .certificate
            .ok_or(AcmeManagedError::Protocol(AcmeError::MissingField))?;
        let certificate_pem = client
            .download_certificate(&certificate_url)
            .map_err(AcmeManagedError::Protocol)?;
        let (mut material, candidate) = self.certificate_material(&csr, &certificate_pem)?;
        material
            .metadata
            .revision
            .clone_from(&candidate.metadata().revision);
        let (renewal_information, renewal_information_status) =
            match client.renewal_information(&material.certificate_pem) {
                Ok(Some(information)) => {
                    let usable = material
                        .metadata
                        .not_before_unix_seconds
                        .zip(material.metadata.not_after_unix_seconds)
                        .is_some_and(|(not_before, not_after)| {
                            stable_renewal_time_in_window(
                                information.suggested_window_start_unix_seconds,
                                information.suggested_window_end_unix_seconds,
                                not_before,
                                not_after,
                                &self.certificate,
                            )
                            .is_some()
                        });
                    (
                        Some(information),
                        if usable { "applied" } else { "invalid" },
                    )
                }
                Ok(None) => (None, "not_advertised"),
                Err(AcmeError::InvalidRenewalInformation) => (None, "invalid"),
                Err(_) => (None, "unavailable"),
            };
        let revision = material.metadata.revision.clone();
        self.revisions
            .commit(&self.certificate, &revision, &material)
            .map_err(AcmeManagedError::State)?;
        let expected = self.active.snapshot();
        let outcome = if candidate.metadata().revision == expected.metadata().revision {
            AcmeManagedOutcome::Unchanged
        } else {
            self.active
                .publish_if_current(&expected, Arc::new(candidate))
                .map_err(|_| AcmeManagedError::Publication)?;
            AcmeManagedOutcome::Activated
        };
        self.update_schedule(
            material.metadata.not_before_unix_seconds,
            material.metadata.not_after_unix_seconds,
            revision,
            outcome.code(),
            client.account().map(|account| account.url.clone()),
            client.directory().document.renewal_info.clone(),
            renewal_information.as_ref(),
            renewal_information_status,
        )
        .map_err(AcmeManagedError::State)?;
        Ok(outcome)
    }

    pub(super) fn client_with_account<T: AcmeTransport>(
        &self,
        transport: T,
        register_if_missing: bool,
        operation: AcmeOperation,
    ) -> Result<AcmeClient<T>, AcmeManagedError> {
        let key_algorithm = account_key_algorithm(self.policy.key_type);
        let account_key_path = format!("accounts/{}/account-key.pem", self.certificate);
        let account_key = match self
            .revisions
            .state()
            .read_bounded(&account_key_path, oxiroute_acme::MAX_CERTIFICATE_BYTES)
        {
            Ok(bytes) => {
                AccountKey::from_pem(SecretBytes::new(bytes)).map_err(AcmeManagedError::Protocol)?
            }
            Err(AcmeStateError::FileOpen(error))
                if error.kind() == io::ErrorKind::NotFound && register_if_missing =>
            {
                let key =
                    AccountKey::generate(key_algorithm).map_err(AcmeManagedError::Protocol)?;
                self.revisions
                    .state()
                    .write_secret(&account_key_path, key.private_key_pem())
                    .map_err(AcmeManagedError::State)?;
                key
            }
            Err(AcmeStateError::FileOpen(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Err(AcmeManagedError::AccountNotConfigured);
            }
            Err(error) => return Err(AcmeManagedError::State(error)),
        };
        let origin =
            OriginPolicy::strict(&self.policy.directory_url).map_err(AcmeManagedError::Protocol)?;
        let mut client = AcmeClient::new_with_operation(
            transport,
            &self.policy.directory_url,
            origin,
            account_key,
            Arc::new(SystemClock),
            operation,
        )
        .map_err(AcmeManagedError::Protocol)?;
        let account_path = format!("accounts/{}/account.json", self.certificate);
        match self
            .revisions
            .state()
            .read_json::<PersistedAccount>(&account_path, oxiroute_acme::MAX_JOB_BYTES)
        {
            Ok(persisted) => {
                if persisted.directory_url != self.policy.directory_url {
                    return Err(AcmeManagedError::AccountDirectoryChanged);
                }
                client
                    .set_account(persisted.account)
                    .map_err(AcmeManagedError::Protocol)?;
            }
            Err(AcmeStateError::FileOpen(error))
                if error.kind() == io::ErrorKind::NotFound && register_if_missing =>
            {
                let account = client
                    .register_account(&oxiroute_acme::AccountRequest {
                        contacts: self.policy.contacts.clone(),
                        terms_agreed: self.policy.terms_agreed,
                    })
                    .map_err(AcmeManagedError::Protocol)?;
                self.revisions
                    .state()
                    .write_json(
                        &account_path,
                        &PersistedAccount {
                            directory_url: self.policy.directory_url.clone(),
                            account,
                        },
                    )
                    .map_err(AcmeManagedError::State)?;
            }
            Err(AcmeStateError::FileOpen(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Err(AcmeManagedError::AccountNotConfigured);
            }
            Err(error) => return Err(AcmeManagedError::State(error)),
        }
        Ok(client)
    }
}
