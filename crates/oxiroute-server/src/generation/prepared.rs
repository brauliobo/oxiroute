#[allow(clippy::wildcard_imports)]
use super::*;

pub struct PreparedGeneration {
    pub(super) config: Arc<ValidatedConfig>,
    pub(super) inventory: ListenerInventory,
    pub(super) metrics: RuntimeMetrics,
    pub(super) resources: GenerationResources,
    pub(super) revision: GenerationRevision,
}

impl PreparedGeneration {
    fn prepare_runtime_components(
        config: &Arc<ValidatedConfig>,
        transaction: &mut PreparedGenerationTransaction,
        deadline: Option<Instant>,
    ) -> Result<(), GenerationError> {
        transaction.plan =
            Some(runtime_plan(config).map_err(|source| GenerationError::Plan(Box::new(source)))?);
        #[cfg(test)]
        transaction
            .drop_probes
            .push(PreparedTransactionDropProbe::new("plan"));
        transaction.acquired = Some(
            acquire_runtime_services_with_deadline(
                transaction.plan.as_ref().expect("provisional runtime plan"),
                deadline,
            )
            .map_err(GenerationError::from)?,
        );
        #[cfg(test)]
        transaction
            .drop_probes
            .push(PreparedTransactionDropProbe::new("acquisition"));
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn prepare_rtmp(
        services: &[crate::ServiceSpec],
    ) -> Result<PreparedRtmpRuntimeSet, GenerationError> {
        Self::prepare_rtmp_with_deadline(services, RtmpPrepareMode::Activation, None)
    }

    fn prepare_rtmp_with_deadline(
        services: &[crate::ServiceSpec],
        mode: RtmpPrepareMode,
        deadline: Option<Instant>,
    ) -> Result<PreparedRtmpRuntimeSet, GenerationError> {
        let mut plans = Vec::new();
        for service in services {
            let ServiceKind::Rtmp(service) = &service.kind else {
                continue;
            };
            if !plans.iter().any(|plan: &oxiroute_rtmp::RtmpServicePlan| {
                plan.service_id() == service.service_id()
            }) {
                #[cfg(test)]
                if crate::service_plan::trace_staged_rtmp_prepare()
                    != crate::service_plan::RtmpRuntimeFault::None
                {
                    return Err(GenerationError::RuntimePrepare);
                }
                plans.push(service.value_plan());
            }
        }
        let listener_addresses = services.iter().filter_map(|service| match service.bind {
            oxiroute_config::ListenerBind::Socket { address }
            | oxiroute_config::ListenerBind::Udp { address } => Some(address),
            oxiroute_config::ListenerBind::Unix { .. } => None,
        });
        let context = RtmpPrepareContext::new(mode, listener_addresses);
        let deadline = deadline.unwrap_or_else(|| Instant::now() + GENERATION_PREPARATION_TIMEOUT);
        let prepared = PreparedRtmpRuntimeSet::prepare(plans, &context, deadline)
            .map_err(map_rtmp_runtime_set_error)?;
        Ok(prepared)
    }

    fn check_generation_inputs(
        config: &ValidatedConfig,
        tls: &crate::PreparedTls,
    ) -> Result<(), GenerationError> {
        let draft = config.as_draft();
        crate::stats::preflight_admin_token(
            draft
                .stats
                .as_ref()
                .and_then(|stats| stats.admin_token_file.as_deref()),
        )?;
        if draft.management.is_some() {
            let token_file = std::env::var_os("OXIROUTE_MANAGEMENT_TOKEN_FILE")
                .map(std::path::PathBuf::from)
                .ok_or(GenerationError::ManagementToken)?;
            crate::rtmp_api::preflight_management_token(&token_file)
                .map_err(|_| GenerationError::ManagementToken)?;
        }
        if let Some(ui_dir) = draft
            .management
            .as_ref()
            .and_then(|management| management.ui_dir.as_deref())
        {
            crate::rtmp_api::UiAssets::load(ui_dir).map_err(|_| GenerationError::RuntimePrepare)?;
        }
        tls.check_certbot_watcher(crate::CertbotWatcherConfig::default())
            .map_err(|_| GenerationError::RuntimePrepare)?;
        tls.check_file_watcher(crate::FileWatcherConfig::default())
            .map_err(|_| GenerationError::RuntimePrepare)?;
        Ok(())
    }

    pub(super) fn prepare_from(
        document: ResolvedConfigDocument,
        source: ListenerSource<'_>,
    ) -> Result<PreparationOutput, GenerationError> {
        Self::prepare_from_with_deadline(document, source, None)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "source-specific acquisition and the common transactional tail remain one auditable state machine"
    )]
    pub(super) fn prepare_from_with_deadline(
        document: ResolvedConfigDocument,
        source: ListenerSource<'_>,
        deadline: Option<Instant>,
    ) -> Result<PreparationOutput, GenerationError> {
        let revision = GenerationRevision {
            disk: document.authored_revision,
            candidate: document.effective_revision,
        };
        let config = Arc::new(document.validated_config);
        let inventory = ListenerInventory::compile(&config);
        inventory.validate_public_display_names()?;
        let mut transaction = PreparedGenerationTransaction::new();
        let process = match source {
            ListenerSource::BindOrReuse { previous, process } => {
                Self::prepare_runtime_components(&config, &mut transaction, deadline)?;
                transaction.reservations = Some(ListenerReservations::prepare(&config, previous)?);
                #[cfg(test)]
                transaction
                    .drop_probes
                    .push(PreparedTransactionDropProbe::new("reservations"));
                Some(process)
            }
            #[cfg(target_os = "linux")]
            ListenerSource::Adopt {
                descriptors,
                process,
            } => {
                transaction.reservations_precede_plan = true;
                transaction.descriptors = Some(descriptors);
                transaction.reservations = Some(ListenerReservations::adopt(
                    &config,
                    transaction
                        .descriptors
                        .take()
                        .expect("provisional descriptor set"),
                )?);
                #[cfg(test)]
                transaction
                    .drop_probes
                    .push(PreparedTransactionDropProbe::new("reservations"));
                Self::prepare_runtime_components(&config, &mut transaction, deadline)?;
                Some(process)
            }
            ListenerSource::Validate { previous } => {
                transaction.plan = Some(
                    validation_plan(&config)
                        .map_err(|source| GenerationError::Plan(Box::new(source)))?,
                );
                #[cfg(test)]
                transaction
                    .drop_probes
                    .push(PreparedTransactionDropProbe::new("plan"));
                transaction.acquired = Some(
                    validate_runtime_services(
                        transaction
                            .plan
                            .as_ref()
                            .expect("provisional validation plan"),
                    )
                    .map_err(|source| GenerationError::Plan(Box::new(source)))?,
                );
                #[cfg(test)]
                transaction
                    .drop_probes
                    .push(PreparedTransactionDropProbe::new("acquisition"));
                transaction.reservations = Some(ListenerReservations::prepare_for_validation(
                    &config, previous,
                )?);
                #[cfg(test)]
                transaction
                    .drop_probes
                    .push(PreparedTransactionDropProbe::new("reservations"));
                None
            }
        };
        #[cfg(test)]
        fail_after_prepared_transaction_stage(PreparedTransactionFault::Reservations)?;
        #[cfg(not(test))]
        fail_after_prepared_transaction_stage(())?;
        Self::check_generation_inputs(&config, transaction.acquired().tls())?;
        let metrics = if let Some(process) = process {
            let metrics = RuntimeMetrics::for_process(process);
            transaction.listener_registration =
                Some(metrics.register_inventory(inventory.entries())?);
            #[cfg(test)]
            transaction
                .drop_probes
                .push(PreparedTransactionDropProbe::new("listener_registration"));
            #[cfg(test)]
            fail_after_prepared_transaction_stage(PreparedTransactionFault::ListenerRegistration)?;
            #[cfg(not(test))]
            fail_after_prepared_transaction_stage(())?;
            metrics.register_upstream_pools(transaction.acquired().pools().iter().cloned())?;
            Some(metrics)
        } else {
            None
        };
        transaction.rtmp = Some(Self::prepare_rtmp_with_deadline(
            transaction.acquired().services(),
            if metrics.is_some() {
                RtmpPrepareMode::Activation
            } else {
                RtmpPrepareMode::Validation
            },
            deadline,
        )?);
        #[cfg(test)]
        transaction
            .drop_probes
            .push(PreparedTransactionDropProbe::new("rtmp_prepared"));
        #[cfg(test)]
        fail_after_prepared_transaction_stage(PreparedTransactionFault::RtmpPreparation)?;
        #[cfg(not(test))]
        fail_after_prepared_transaction_stage(())?;
        let Some(metrics) = metrics else {
            return Ok(PreparationOutput::Validated);
        };
        metrics.set_rtmp_recording_supported(
            transaction
                .plan
                .as_ref()
                .expect("provisional runtime plan")
                .rtmp_recording_supported,
        );
        Ok(PreparationOutput::Prepared(Self {
            config,
            inventory,
            metrics,
            resources: transaction.commit(),
            revision,
        }))
    }

    #[must_use]
    pub fn revision(&self) -> &GenerationRevision {
        &self.revision
    }
}
