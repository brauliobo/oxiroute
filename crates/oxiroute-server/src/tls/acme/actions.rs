#[allow(clippy::wildcard_imports)]
use super::*;

impl AcmeManagedReconciler {
    /// Runs one bounded managed issuance or renewal using the production HTTPS transport.
    ///
    /// # Errors
    ///
    /// Returns a redacted managed-job error when account, challenge, ACME, storage, or publication
    /// steps fail.
    pub fn renew_now(&self) -> Result<AcmeManagedOutcome, AcmeManagedError> {
        self.renew_now_with_correlation("system")
    }

    /// Runs one bounded managed issuance or renewal and records the caller correlation ID.
    ///
    /// # Errors
    ///
    /// Returns a redacted managed-job error when account, challenge, ACME, storage, or publication
    /// steps fail.
    pub fn renew_now_with_correlation(
        &self,
        correlation_id: impl Into<String>,
    ) -> Result<AcmeManagedOutcome, AcmeManagedError> {
        let correlation_id = correlation_id.into();
        self.renew_with_provider(
            SystemAcmeTransport::default(),
            self.dns_provider.clone(),
            &correlation_id,
            AcmeOperation::default(),
        )
    }

    #[doc(hidden)]
    pub fn renew_now_with_operation(
        &self,
        operation: AcmeOperation,
    ) -> Result<AcmeManagedOutcome, AcmeManagedError> {
        self.renew_with_provider(
            SystemAcmeTransport::default(),
            self.dns_provider.clone(),
            "system",
            operation,
        )
    }

    /// Runs one bounded managed issuance or renewal with an injected transport.
    ///
    /// The injected form is used by deterministic local ACME tests and never changes the
    /// production transport used by [`Self::renew_now`].
    ///
    /// # Errors
    ///
    /// Returns a redacted managed-job error when account, challenge, ACME, storage, or publication
    /// steps fail.
    pub fn renew_with_transport<T: AcmeTransport>(
        &self,
        transport: T,
    ) -> Result<AcmeManagedOutcome, AcmeManagedError> {
        self.renew_with_provider(
            transport,
            self.dns_provider.clone(),
            "test",
            AcmeOperation::default(),
        )
    }

    #[cfg(test)]
    pub(super) fn renew_with_transport_and_operation<T: AcmeTransport>(
        &self,
        transport: T,
        operation: AcmeOperation,
    ) -> Result<AcmeManagedOutcome, AcmeManagedError> {
        self.renew_with_provider(transport, self.dns_provider.clone(), "test", operation)
    }

    /// Requests cooperative cancellation of the currently running job.
    ///
    /// # Errors
    ///
    /// Returns [`AcmeManagedError::NoJob`] when no managed job is running.
    pub fn cancel_job(&self) -> Result<String, AcmeManagedError> {
        let control = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(id) = control.id.clone() else {
            return Err(AcmeManagedError::NoJob);
        };
        if let Some(cancellation) = &control.cancellation {
            cancellation.cancel();
        }
        Ok(id)
    }

    /// Pauses automatic issuance and cooperatively stops the current job, if any.
    ///
    /// # Errors
    ///
    /// Returns a state error when the durable pause marker cannot be written.
    pub fn pause(&self) -> Result<Option<String>, AcmeManagedError> {
        let id = {
            let control = self
                .control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(cancellation) = &control.cancellation {
                cancellation.cancel();
            }
            control.id.clone()
        };
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.paused = true;
            state.job_status = Some(JobStatus::Paused);
            state.next_action_unix_seconds = None;
        }
        self.persist_renewal().map_err(AcmeManagedError::State)?;
        Ok(id)
    }

    /// Resumes automatic issuance using the current certificate schedule.
    ///
    /// # Errors
    ///
    /// Returns a state error when the durable pause marker cannot be written.
    pub fn resume(&self) -> Result<(), AcmeManagedError> {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.paused = false;
            state.job_status = None;
            state.next_action_unix_seconds = restored_next_action(&state, &self.certificate);
        }
        self.persist_renewal().map_err(AcmeManagedError::State)
    }

    /// Revokes the currently selected leaf certificate without removing its active material.
    ///
    /// # Errors
    ///
    /// Returns a redacted managed-job error when the account, certificate, ACME endpoint, or state
    /// operation fails.
    pub fn revoke_now_with_correlation(
        &self,
        reason: Option<u8>,
        correlation_id: impl Into<String>,
    ) -> Result<(AcmeManagedOutcome, String), AcmeManagedError> {
        let _job = self.try_lock_job()?;
        let correlation_id = correlation_id.into();
        let job_id = self.begin_action_job("revoke", &correlation_id)?;
        let result = (|| {
            let material = self
                .revisions
                .load_current(&self.certificate)
                .map_err(AcmeManagedError::State)?;
            let mut client = self.client_with_account(
                SystemAcmeTransport::default(),
                false,
                AcmeOperation::default(),
            )?;
            client
                .revoke_certificate(&material.certificate_pem, reason)
                .map_err(AcmeManagedError::Protocol)
        })();
        let outcome = AcmeManagedOutcome::Revoked;
        let finish_result = self.finish_action_job(&job_id, &result, outcome);
        self.set_job_id(None);
        finish_result?;
        result.map(|()| (outcome, job_id))
    }

    /// Rolls the account key and persists it only after the ACME server accepts the nested JWS.
    ///
    /// # Errors
    ///
    /// Returns a redacted managed-job error when the account, key, ACME endpoint, or state
    /// operation fails.
    pub fn rollover_account_key_with_correlation(
        &self,
        correlation_id: impl Into<String>,
    ) -> Result<(AcmeManagedOutcome, String), AcmeManagedError> {
        let _job = self.try_lock_job()?;
        let correlation_id = correlation_id.into();
        let job_id = self.begin_action_job("account_rollover", &correlation_id)?;
        let result = (|| {
            let new_key = AccountKey::generate(account_key_algorithm(self.policy.key_type))
                .map_err(AcmeManagedError::Protocol)?;
            let mut client = self.client_with_account(
                SystemAcmeTransport::default(),
                false,
                AcmeOperation::default(),
            )?;
            let account = client
                .rollover_account_key(new_key.clone())
                .map_err(AcmeManagedError::Protocol)?;
            self.revisions
                .state()
                .write_secret(
                    &format!("accounts/{}/account-key.pem", self.certificate),
                    new_key.private_key_pem(),
                )
                .map_err(AcmeManagedError::State)?;
            self.revisions
                .state()
                .write_json(
                    &format!("accounts/{}/account.json", self.certificate),
                    &PersistedAccount {
                        directory_url: self.policy.directory_url.clone(),
                        account,
                    },
                )
                .map_err(AcmeManagedError::State)
        })();
        let outcome = AcmeManagedOutcome::AccountKeyRolledOver;
        let finish_result = self.finish_action_job(&job_id, &result, outcome);
        self.set_job_id(None);
        finish_result?;
        result.map(|()| (outcome, job_id))
    }

    /// Deletes persisted managed ACME state after the caller has removed active references.
    ///
    /// # Errors
    ///
    /// Returns a redacted managed-job error when the state cannot be deleted.
    pub fn delete_state_with_correlation(
        &self,
        correlation_id: impl Into<String>,
    ) -> Result<(AcmeManagedOutcome, String), AcmeManagedError> {
        let _job = self.try_lock_job()?;
        let correlation_id = correlation_id.into();
        let job_id = self.begin_action_job("delete", &correlation_id)?;
        let result = self
            .revisions
            .delete_certificate_state(&self.certificate)
            .map_err(AcmeManagedError::State);
        let outcome = AcmeManagedOutcome::Deleted;
        let finish_result = self.finish_action_job(&job_id, &result, outcome);
        self.set_job_id(None);
        finish_result?;
        result.map(|()| (outcome, job_id))
    }
}
