#[allow(clippy::wildcard_imports)]
use super::*;

impl ManagementState {
    pub(super) fn tls(&self) -> ApiResponse {
        let active = match self.active() {
            Ok(active) => active,
            Err(response) => return response,
        };
        let Ok(snapshot) = active.metrics().snapshot() else {
            return ApiResponse::error(503, "tls_status_unavailable", "TLS status is unavailable");
        };
        let certificates = active
            .config()
            .as_draft()
            .certificates
            .iter()
            .map(|certificate| match &certificate.source {
                CertificateSource::Files { .. } => TlsCertificateDto::files(
                    certificate.name.clone(),
                    certificate.dns_names.clone(),
                    snapshot
                        .direct_file_certificates
                        .iter()
                        .find(|status| status.name == certificate.name)
                        .cloned(),
                ),
                CertificateSource::Certbot { .. } => TlsCertificateDto::certbot(
                    certificate.name.clone(),
                    certificate.dns_names.clone(),
                    snapshot
                        .certbot_certificates
                        .iter()
                        .find(|status| status.name == certificate.name)
                        .cloned(),
                ),
                CertificateSource::AcmeManaged { .. } => TlsCertificateDto::managed(
                    certificate.name.clone(),
                    certificate.dns_names.clone(),
                    active
                        .tls()
                        .acme_reconcilers()
                        .iter()
                        .find_map(|reconciler| {
                            (reconciler.status().certificate == certificate.name)
                                .then(|| reconciler.status())
                        }),
                ),
                CertificateSource::SelfSignedDevelopment { .. } => {
                    let status = active
                        .tls()
                        .certificates()
                        .get(&certificate.name)
                        .map(|active| {
                            let metadata = active.snapshot();
                            (
                                metadata.metadata().revision.clone(),
                                metadata.metadata().validity.not_after.clone(),
                            )
                        });
                    let (revision, expires_at) = status.unzip();
                    TlsCertificateDto::self_signed(
                        certificate.name.clone(),
                        certificate.dns_names.clone(),
                        revision,
                        expires_at,
                    )
                }
            })
            .collect::<Vec<_>>();
        dto_response(
            200,
            &TlsInventoryResponse::new(certificates, snapshot.certbot_watcher),
        )
    }

    pub(super) async fn tls_reconcile(&self, session: &mut ServerSession) -> ApiResponse {
        let request: TlsRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
        };
        let active = mutation.generation();
        let reconcilers = active.tls().certbot_reconcilers();
        let managed_reconcilers = active.tls().acme_reconcilers();
        if let Some(name) = request.certificate.as_deref()
            && !reconcilers
                .iter()
                .any(|reconciler| reconciler.status().certificate == name)
            && !managed_reconcilers
                .iter()
                .any(|reconciler| reconciler.status().certificate == name)
        {
            return ApiResponse::error(404, "certificate_not_found", "certificate was not found");
        }
        let mut outcomes = Vec::new();
        for reconciler in reconcilers {
            let status = reconciler.status();
            if request
                .certificate
                .as_deref()
                .is_none_or(|name| name == status.certificate)
            {
                match reconciler.reconcile() {
                    Ok(outcome) => {
                        let (previous, archive) = match outcome {
                            crate::CertbotReconcileOutcome::Unchanged { archive_revision } => {
                                (None, archive_revision)
                            }
                            crate::CertbotReconcileOutcome::Activated {
                                previous_archive_revision,
                                archive_revision,
                                ..
                            } => (Some(previous_archive_revision), archive_revision),
                        };
                        outcomes.push(TlsReconcileOutcome::certbot(
                            status,
                            outcome.code(),
                            previous.map(|value| value.to_string()),
                            archive.to_string(),
                        ));
                    }
                    Err(error) => {
                        return ApiResponse::error(
                            503,
                            error.code(),
                            "certificate reconciliation failed",
                        );
                    }
                }
            }
        }
        for reconciler in managed_reconcilers {
            let status = reconciler.status();
            if request
                .certificate
                .as_deref()
                .is_none_or(|name| name == status.certificate)
            {
                match reconciler.reconcile() {
                    Ok(outcome) => outcomes.push(TlsReconcileOutcome::managed(
                        reconciler.status(),
                        outcome.code(),
                    )),
                    Err(_) => {
                        return ApiResponse::error(
                            503,
                            "managed_certificate_reconciliation_failed",
                            "managed certificate reconciliation failed",
                        );
                    }
                }
            }
        }
        dto_response(200, &TlsReconcileResponse::new(outcomes))
    }

    pub(super) async fn tls_renew(
        &self,
        session: &mut ServerSession,
        context: &AuditContext,
    ) -> ApiResponse {
        let request: TlsRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
        };
        let reconciler = {
            let active = mutation.generation();
            active
                .tls()
                .acme_reconcilers()
                .iter()
                .find(|reconciler| {
                    request
                        .certificate
                        .as_deref()
                        .is_none_or(|name| name == reconciler.status().certificate)
                })
                .cloned()
        };
        let Some(reconciler) = reconciler else {
            return ApiResponse::error(
                404,
                "managed_certificate_not_found",
                "managed ACME certificate was not found",
            );
        };
        let certificate_name = reconciler.status().certificate;
        crate::observability::operational_event::emit_certificate_with_context(
            "certificate_renewal",
            "requested",
            &certificate_name,
            context,
        );
        let worker_reconciler = Arc::clone(&reconciler);
        let correlation_id = context.correlation_id.clone();
        let result = tokio::task::spawn_blocking(move || {
            let result = worker_reconciler.renew_now_with_correlation(correlation_id);
            drop(mutation);
            result
        })
        .await;
        match result {
            Ok(Ok(outcome)) => {
                let status = reconciler.status();
                crate::observability::operational_event::emit_certificate_with_context(
                    if outcome == crate::AcmeManagedOutcome::Activated {
                        "certificate_activated"
                    } else {
                        "certificate_renewal"
                    },
                    if outcome == crate::AcmeManagedOutcome::Activated {
                        "activated"
                    } else {
                        "applied"
                    },
                    &status.certificate,
                    context,
                );
                dto_response(200, &TlsRenewResponse::new(status, outcome.code()))
            }
            Ok(Err(error)) => {
                crate::observability::operational_event::emit_certificate_with_context(
                    "certificate_renewal",
                    "failed",
                    &certificate_name,
                    context,
                );
                ApiResponse::error(503, error.code(), "managed ACME renewal failed")
            }
            Err(_) => ApiResponse::error(503, "renewal_worker_failed", "renewal worker failed"),
        }
    }

    pub(super) async fn tls_revoke(
        &self,
        session: &mut ServerSession,
        context: &AuditContext,
    ) -> ApiResponse {
        let request: TlsRevokeRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
        };
        let reconciler = {
            let active = mutation.generation();
            active
                .tls()
                .acme_reconcilers()
                .iter()
                .find(|reconciler| reconciler.status().certificate == request.certificate)
                .cloned()
        };
        let Some(reconciler) = reconciler else {
            return ApiResponse::error(
                404,
                "managed_certificate_not_found",
                "managed ACME certificate was not found",
            );
        };
        let certificate = reconciler.status().certificate;
        crate::observability::operational_event::emit_certificate_with_context(
            "certificate_revocation",
            "requested",
            &certificate,
            context,
        );
        let worker_reconciler = Arc::clone(&reconciler);
        let correlation_id = context.correlation_id.clone();
        let reason = request.reason;
        let result = tokio::task::spawn_blocking(move || {
            let result = worker_reconciler.revoke_now_with_correlation(reason, correlation_id);
            drop(mutation);
            result
        })
        .await;
        match result {
            Ok(Ok((outcome, job_id))) => {
                crate::observability::operational_event::emit_certificate_with_context(
                    "certificate_revocation",
                    "applied",
                    &certificate,
                    context,
                );
                dto_response(
                    200,
                    &TlsActionResponse::new(certificate, outcome.code(), Some(job_id)),
                )
            }
            Ok(Err(error)) => {
                crate::observability::operational_event::emit_certificate_with_context(
                    "certificate_revocation",
                    "failed",
                    &certificate,
                    context,
                );
                managed_error_response(error, "managed ACME revocation failed")
            }
            Err(_) => {
                ApiResponse::error(503, "revocation_worker_failed", "revocation worker failed")
            }
        }
    }

    pub(super) async fn tls_delete(
        &self,
        session: &mut ServerSession,
        context: &AuditContext,
    ) -> ApiResponse {
        let request: TlsRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let Some(certificate) = request.certificate.as_deref() else {
            return ApiResponse::error(
                400,
                "certificate_required",
                "a managed certificate is required",
            );
        };
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
        };
        let active = mutation.generation();
        if active
            .config()
            .as_draft()
            .tls_profiles
            .iter()
            .any(|profile| {
                profile.default_certificate == certificate
                    || profile.certificates.iter().any(|name| name == certificate)
            })
        {
            return ApiResponse::error(
                409,
                "certificate_in_use",
                "managed certificate is still referenced by an active TLS profile",
            );
        }
        let reconciler = active
            .tls()
            .acme_reconcilers()
            .iter()
            .find(|reconciler| reconciler.status().certificate == certificate)
            .cloned();
        let Some(reconciler) = reconciler else {
            return ApiResponse::error(
                404,
                "managed_certificate_not_found",
                "managed ACME certificate was not found",
            );
        };
        let certificate = certificate.to_owned();
        crate::observability::operational_event::emit_certificate_with_context(
            "certificate_deletion",
            "requested",
            &certificate,
            context,
        );
        let worker_reconciler = Arc::clone(&reconciler);
        let correlation_id = context.correlation_id.clone();
        let result = tokio::task::spawn_blocking(move || {
            let result = worker_reconciler.delete_state_with_correlation(correlation_id);
            drop(mutation);
            result
        })
        .await;
        match result {
            Ok(Ok((outcome, job_id))) => {
                crate::observability::operational_event::emit_certificate_with_context(
                    "certificate_deletion",
                    "applied",
                    &certificate,
                    context,
                );
                dto_response(
                    200,
                    &TlsActionResponse::new(certificate, outcome.code(), Some(job_id)),
                )
            }
            Ok(Err(error)) => {
                crate::observability::operational_event::emit_certificate_with_context(
                    "certificate_deletion",
                    "failed",
                    &certificate,
                    context,
                );
                managed_error_response(error, "managed ACME state deletion failed")
            }
            Err(_) => ApiResponse::error(503, "deletion_worker_failed", "deletion worker failed"),
        }
    }

    pub(super) async fn tls_account_rollover(
        &self,
        session: &mut ServerSession,
        context: &AuditContext,
    ) -> ApiResponse {
        let request: TlsRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
        };
        let reconciler = {
            let active = mutation.generation();
            active
                .tls()
                .acme_reconcilers()
                .iter()
                .find(|reconciler| {
                    request
                        .certificate
                        .as_deref()
                        .is_none_or(|name| name == reconciler.status().certificate)
                })
                .cloned()
        };
        let Some(reconciler) = reconciler else {
            return ApiResponse::error(
                404,
                "managed_certificate_not_found",
                "managed ACME certificate was not found",
            );
        };
        let certificate = reconciler.status().certificate;
        crate::observability::operational_event::emit_certificate_with_context(
            "certificate_account_rollover",
            "requested",
            &certificate,
            context,
        );
        let worker_reconciler = Arc::clone(&reconciler);
        let correlation_id = context.correlation_id.clone();
        let result = tokio::task::spawn_blocking(move || {
            let result = worker_reconciler.rollover_account_key_with_correlation(correlation_id);
            drop(mutation);
            result
        })
        .await;
        match result {
            Ok(Ok((outcome, job_id))) => {
                crate::observability::operational_event::emit_certificate_with_context(
                    "certificate_account_rollover",
                    "applied",
                    &certificate,
                    context,
                );
                dto_response(
                    200,
                    &TlsActionResponse::new(certificate, outcome.code(), Some(job_id)),
                )
            }
            Ok(Err(error)) => {
                crate::observability::operational_event::emit_certificate_with_context(
                    "certificate_account_rollover",
                    "failed",
                    &certificate,
                    context,
                );
                managed_error_response(error, "managed ACME account rollover failed")
            }
            Err(_) => ApiResponse::error(
                503,
                "account_rollover_worker_failed",
                "account rollover worker failed",
            ),
        }
    }

    pub(super) async fn tls_job_control(
        &self,
        session: &mut ServerSession,
        context: &AuditContext,
        control: JobControl,
    ) -> ApiResponse {
        let request: TlsRequest = match body(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let mutation = match begin_mutation(&self.generations, &request.expected_active_revision) {
            Ok(mutation) => mutation,
            Err(response) => return response,
        };
        let reconciler = {
            let active = mutation.generation();
            active
                .tls()
                .acme_reconcilers()
                .iter()
                .find(|reconciler| {
                    request
                        .certificate
                        .as_deref()
                        .is_none_or(|name| name == reconciler.status().certificate)
                })
                .cloned()
        };
        drop(mutation);
        let Some(reconciler) = reconciler else {
            return ApiResponse::error(
                404,
                "managed_certificate_not_found",
                "managed ACME certificate was not found",
            );
        };
        let certificate = reconciler.status().certificate;
        let result = match control {
            JobControl::Cancel => reconciler.cancel_job().map(|job_id| {
                crate::observability::operational_event::emit_certificate_with_context(
                    "certificate_job_control",
                    "requested",
                    &certificate,
                    context,
                );
                TlsJobControlResponse::cancellation(certificate.clone(), job_id)
            }),
            JobControl::Pause => reconciler.pause().map(|job_id| {
                crate::observability::operational_event::emit_certificate_with_context(
                    "certificate_job_control",
                    "applied",
                    &certificate,
                    context,
                );
                TlsJobControlResponse::paused(certificate.clone(), job_id)
            }),
            JobControl::Resume => reconciler.resume().map(|()| {
                crate::observability::operational_event::emit_certificate_with_context(
                    "certificate_job_control",
                    "applied",
                    &certificate,
                    context,
                );
                TlsJobControlResponse::resumed(certificate.clone())
            }),
        };
        match result {
            Ok(response) => dto_response(202, &response),
            Err(error) => managed_error_response(error, "managed ACME job control failed"),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum JobControl {
    Cancel,
    Pause,
    Resume,
}

#[allow(clippy::needless_pass_by_value)]
pub(super) fn managed_error_response(error: crate::AcmeManagedError, message: &str) -> ApiResponse {
    let status = match &error {
        crate::AcmeManagedError::Protocol(oxiroute_acme::AcmeError::InvalidRevocationReason) => 400,
        crate::AcmeManagedError::State(oxiroute_acme::AcmeStateError::PendingDnsCleanup)
        | crate::AcmeManagedError::Busy
        | crate::AcmeManagedError::Paused
        | crate::AcmeManagedError::NoJob => 409,
        _ => 503,
    };
    ApiResponse::error(status, error.code(), message)
}
