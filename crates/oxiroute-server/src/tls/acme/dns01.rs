#[allow(clippy::wildcard_imports)]
use super::*;

impl AcmeManagedReconciler {
    /// Attempts one bounded recovery of a durable DNS-01 cleanup journal.
    #[must_use]
    pub fn recover_pending_dns_cleanup(&self) -> AcmeDnsCleanupRecovery {
        self.recover_pending_dns_cleanup_with_operation(&AcmeOperation::default())
            .unwrap_or(AcmeDnsCleanupRecovery::Deferred)
    }

    pub(super) fn recover_pending_dns_cleanup_with_operation(
        &self,
        operation: &AcmeOperation,
    ) -> Result<AcmeDnsCleanupRecovery, AcmeManagedError> {
        if self.policy.challenge != AcmeChallengeType::Dns01 {
            return Ok(AcmeDnsCleanupRecovery::NotPending);
        }
        let journal = match self
            .revisions
            .state()
            .read_secret_json::<PersistedDnsCleanup>(
                &dns_cleanup_path(&self.certificate),
                MAX_JOB_BYTES,
            ) {
            Ok(journal) => journal,
            Err(AcmeStateError::FileOpen(error)) if error.kind() == io::ErrorKind::NotFound => {
                let cleanup_failed = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .dns_cleanup_status
                    == "failed";
                return Ok(if cleanup_failed {
                    AcmeDnsCleanupRecovery::Deferred
                } else {
                    AcmeDnsCleanupRecovery::NotPending
                });
            }
            Err(_) => {
                self.set_dns_status("failed", "degraded");
                return Ok(AcmeDnsCleanupRecovery::Deferred);
            }
        };
        let Some(provider) = self.dns_provider.as_ref() else {
            self.set_dns_status("failed", "unsupported");
            return Ok(AcmeDnsCleanupRecovery::Deferred);
        };
        let Some(dns01) = self.policy.dns01.as_ref() else {
            self.set_dns_status("failed", "unsupported");
            return Ok(AcmeDnsCleanupRecovery::Deferred);
        };
        if journal.provider != dns01.provider || provider.name() != journal.provider {
            self.set_dns_status("failed", "degraded");
            return Ok(AcmeDnsCleanupRecovery::Deferred);
        }
        let Ok(record) = persisted_dns_record(&self.certificate, &journal) else {
            self.set_dns_status("failed", "degraded");
            return Ok(AcmeDnsCleanupRecovery::Deferred);
        };
        let Ok(credentials) = load_dns_credentials(&dns01.credential_file, &self.certificate)
        else {
            self.set_dns_status("failed", "degraded");
            return Ok(AcmeDnsCleanupRecovery::Deferred);
        };
        operation.check().map_err(AcmeManagedError::Protocol)?;
        let timeout = operation.remaining(Duration::from_secs(dns01.timeout_seconds));
        let Ok(cleanup_operation) =
            Dns01Operation::with_cancellation(timeout, operation.cancellation().clone())
        else {
            self.set_dns_status("failed", "degraded");
            return Ok(AcmeDnsCleanupRecovery::Deferred);
        };
        match provider.cleanup_txt_record(&record, &credentials, &cleanup_operation) {
            Ok(()) => {}
            Err(Dns01ProviderError::Cancelled) => {
                return Err(AcmeManagedError::Protocol(AcmeError::Cancelled));
            }
            Err(_) => {
                self.set_dns_status("failed", "degraded");
                return Ok(AcmeDnsCleanupRecovery::Deferred);
            }
        }
        if self
            .revisions
            .state()
            .remove_file(&dns_cleanup_path(&self.certificate))
            .is_err()
        {
            self.set_dns_status("failed", "degraded");
            return Ok(AcmeDnsCleanupRecovery::Deferred);
        }
        self.set_dns_status("recovered", "healthy");
        Ok(AcmeDnsCleanupRecovery::Recovered)
    }

    #[allow(
        clippy::needless_pass_by_value,
        clippy::too_many_arguments,
        clippy::unused_self
    )]
    pub(super) fn complete_dns01_challenge<T: AcmeTransport>(
        &self,
        client: &mut AcmeClient<T>,
        authorization_url: &str,
        challenge: &Dns01Challenge,
        provider: &dyn Dns01Provider,
        credentials: &Dns01Credentials,
        timeout_seconds: u64,
        acme_operation: &AcmeOperation,
    ) -> Result<(), AcmeManagedError> {
        acme_operation.check().map_err(AcmeManagedError::Protocol)?;
        let timeout = acme_operation.remaining(Duration::from_secs(timeout_seconds));
        let operation =
            Dns01Operation::with_cancellation(timeout, acme_operation.cancellation().clone())
                .map_err(|error| {
                    self.set_dns_status("none", "degraded");
                    AcmeManagedError::DnsProvider(error)
                })?;
        operation.check().map_err(|error| {
            self.set_dns_status("none", "degraded");
            AcmeManagedError::DnsProvider(error)
        })?;
        let record = provider
            .create_txt_record(challenge, credentials, &operation)
            .map_err(|error| {
                self.set_dns_status("none", "degraded");
                AcmeManagedError::DnsProvider(error)
            })?;
        if !record.matches(challenge, provider.name()) {
            self.set_dns_status("none", "degraded");
            return Err(AcmeManagedError::DnsProvider(
                Dns01ProviderError::InvalidRecord,
            ));
        }
        self.persist_dns_cleanup(challenge, &record)
            .inspect_err(|_| {
                self.set_dns_status("failed", "degraded");
                let cleanup_operation = Dns01Operation::new(timeout);
                if let Ok(cleanup_operation) = cleanup_operation
                    && provider
                        .cleanup_txt_record(&record, credentials, &cleanup_operation)
                        .is_ok()
                {
                    self.set_dns_status("recovered", "healthy");
                }
            })?;
        let authorization_result =
            match provider.wait_for_propagation(challenge, &record, credentials, &operation) {
                Ok(()) => (|| {
                    operation.check().map_err(|error| {
                        self.set_dns_status("failed", "degraded");
                        AcmeManagedError::DnsProvider(error)
                    })?;
                    client
                        .respond_to_dns01_challenge(challenge)
                        .map_err(AcmeManagedError::Protocol)?;
                    let authorization = client
                        .poll_authorization_for(
                            authorization_url,
                            &poll_policy(
                                unix_now().saturating_add(600),
                                Some(acme_operation.cancellation().clone()),
                            ),
                            ChallengeType::Dns01,
                        )
                        .map_err(AcmeManagedError::Protocol)?;
                    if authorization.status != AuthorizationStatus::Valid {
                        return Err(AcmeManagedError::AuthorizationFailed);
                    }
                    Ok(())
                })(),
                Err(error) => {
                    self.set_dns_status("failed", "degraded");
                    Err(AcmeManagedError::DnsProvider(error))
                }
            };

        self.cleanup_dns_record(provider, credentials, &record, timeout, acme_operation)?;
        authorization_result
    }

    fn persist_dns_cleanup(
        &self,
        challenge: &Dns01Challenge,
        record: &oxiroute_acme::Dns01Record,
    ) -> Result<(), AcmeManagedError> {
        self.revisions
            .state()
            .write_secret_json(
                &dns_cleanup_path(&self.certificate),
                &PersistedDnsCleanup {
                    certificate: self.certificate.clone(),
                    provider: record.provider().into(),
                    identifier: challenge.identifier().into(),
                    challenge_url: challenge.challenge_url().into(),
                    record_name: challenge.record_name().into(),
                    record_value: challenge.record_value().into(),
                    provider_record_id: record.provider_record_id().into(),
                },
            )
            .map_err(AcmeManagedError::State)
    }

    fn cleanup_dns_record(
        &self,
        provider: &dyn Dns01Provider,
        credentials: &Dns01Credentials,
        record: &oxiroute_acme::Dns01Record,
        timeout: Duration,
        acme_operation: &AcmeOperation,
    ) -> Result<(), AcmeManagedError> {
        acme_operation.check().map_err(AcmeManagedError::Protocol)?;
        let cleanup_operation = Dns01Operation::with_cancellation(
            acme_operation.remaining(timeout),
            acme_operation.cancellation().clone(),
        )
        .map_err(|error| {
            self.set_dns_status("failed", "degraded");
            AcmeManagedError::DnsCleanup(error)
        })?;
        cleanup_operation.check().map_err(|error| {
            self.set_dns_status("failed", "degraded");
            AcmeManagedError::DnsCleanup(error)
        })?;
        provider
            .cleanup_txt_record(record, credentials, &cleanup_operation)
            .map_err(|error| {
                self.set_dns_status("failed", "degraded");
                if error == Dns01ProviderError::Cancelled {
                    AcmeManagedError::Protocol(AcmeError::Cancelled)
                } else {
                    AcmeManagedError::DnsCleanup(error)
                }
            })?;
        self.revisions
            .state()
            .remove_file(&dns_cleanup_path(&self.certificate))
            .map_err(|error| {
                self.set_dns_status("failed", "degraded");
                AcmeManagedError::State(error)
            })?;
        self.set_dns_status("recovered", "healthy");
        Ok(())
    }

    fn set_dns_status(&self, cleanup_status: &'static str, provider_health: &'static str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.dns_cleanup_status = cleanup_status;
        state.dns_provider_health = provider_health;
    }
}
