#[allow(clippy::wildcard_imports)]
use super::*;

impl RuntimeMetrics {
    /// Samples process, host, traffic, and listener metrics.
    ///
    /// CPU percent is `None` on the first successful sample and whenever no aggregate host CPU tick
    /// elapsed since the previous sample.
    ///
    /// # Errors
    ///
    /// Returns an error when metrics state cannot be read, the system clock is invalid, aggregate
    /// counters overflow, the current platform is unsupported, or an operating-system metric cannot
    /// be read or parsed truthfully.
    pub fn snapshot(&self) -> Result<RuntimeSnapshot, MetricsError> {
        let (traffic, listeners) = self.counter_snapshots()?;
        let upstream_pools = self.upstream_pool_snapshots()?;
        let (mut certbot_certificates, certbot_watcher) = self.certbot_snapshots()?;
        certbot_certificates.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        let mut acme_managed_certificates = self.acme_managed_snapshots()?;
        acme_managed_certificates.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        let (mut direct_file_certificates, direct_file_watcher) = self.direct_file_snapshots()?;
        direct_file_certificates.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        let mut previous_cpu_sample = self
            .inner
            .previous_cpu_sample
            .lock()
            .map_err(|_| MetricsError::StatePoisoned("previous CPU sample"))?;
        let system = sample_system()?;
        let transport_operations = self
            .inner
            .process_runtime
            .inner
            .transport_operations
            .snapshots();
        let access_records = self.inner.process_runtime.access_records_snapshot()?;
        let cpu_percent = (system.process_status.state == ComponentState::Healthy
            && system.host.status.state == ComponentState::Healthy)
            .then(|| cpu_percent(previous_cpu_sample.as_ref(), &system.cpu))
            .transpose()?
            .flatten();
        let sampled_at_unix_ms = unix_time_ms()?;
        let uptime_ms = u64::try_from(
            self.inner
                .process_runtime
                .inner
                .started_at
                .elapsed()
                .as_millis(),
        )
        .map_err(|_| MetricsError::ValueOutOfRange("uptime milliseconds"))?;

        let SystemSample {
            process_status,
            resident_memory_bytes,
            virtual_memory_bytes,
            thread_count,
            open_file_descriptors,
            host,
            cpu,
        } = system;
        let snapshot = RuntimeSnapshot {
            sampled_at_unix_ms,
            uptime_ms,
            generation_age_ms: u64::try_from(
                self.inner.generation_started_at.elapsed().as_millis(),
            )
            .map_err(|_| MetricsError::ValueOutOfRange("generation age milliseconds"))?,
            process: ProcessSnapshot {
                active_connections: self.inner.process_admission.active.load(Ordering::Relaxed),
                administrative_state: AdministrativeState::from_u8(
                    self.inner
                        .process_admission
                        .administrative_state
                        .load(Ordering::Acquire),
                ),
                status: process_status,
                cpu_percent,
                max_connections: self.inner.process_admission.limit(),
                rejected_connections: self
                    .inner
                    .process_admission
                    .rejected
                    .load(Ordering::Relaxed),
                retry_attempts: self
                    .inner
                    .process_admission
                    .retry_attempts
                    .load(Ordering::Relaxed),
                rescues: self.inner.process_admission.rescue_snapshots(),
                resident_memory_bytes,
                virtual_memory_bytes,
                thread_count,
                open_file_descriptors,
            },
            host,
            traffic,
            listeners,
            upstream_pools,
            transport_operations,
            access_records,
            certbot_certificates,
            certbot_watcher,
            acme_managed_certificates,
            direct_file_certificates,
            direct_file_watcher,
        };
        if process_status.state == ComponentState::Healthy
            && snapshot.host.status.state == ComponentState::Healthy
        {
            *previous_cpu_sample = Some(cpu);
        }
        Ok(snapshot)
    }

    pub(crate) fn topology_health_snapshot(&self) -> Result<RuntimeHealthSnapshot, MetricsError> {
        let (_, listeners) = self.counter_snapshots()?;
        Ok(RuntimeHealthSnapshot {
            sampled_at_unix_ms: unix_time_ms()?,
            listeners,
            upstream_pools: self.upstream_pool_snapshots()?,
        })
    }

    /// Samples every registered listener, including internal-only control listeners.
    ///
    /// # Errors
    ///
    /// Returns an error when the listener registry is poisoned.
    pub fn internal_listener_snapshots(&self) -> Result<Vec<ListenerSnapshot>, MetricsError> {
        self.listener_snapshots(false)
    }

    fn upstream_pool_snapshots(&self) -> Result<Vec<PoolHealthSnapshot>, MetricsError> {
        Ok(self
            .inner
            .upstream_pools
            .read()
            .map_err(|_| MetricsError::StatePoisoned("upstream pools"))?
            .iter()
            .map(|pool| pool.health_snapshot())
            .collect())
    }

    fn certbot_snapshots(
        &self,
    ) -> Result<
        (
            Vec<CertbotCertificateSnapshot>,
            Option<CertbotWatcherSnapshot>,
        ),
        MetricsError,
    > {
        let certbot = self
            .inner
            .certbot
            .read()
            .map_err(|_| MetricsError::StatePoisoned("Certbot monitoring"))?;
        let certificates = certbot
            .reconcilers
            .iter()
            .map(|reconciler| {
                let status = reconciler.status();
                CertbotCertificateSnapshot {
                    name: status.certificate,
                    active_archive_revision: status.active_archive_revision,
                    active_content_revision: status.active_content_revision,
                    expires_at: status.not_after,
                    last_outcome: status.last_outcome.map(str::to_owned),
                    last_error_code: status.last_error_code.map(str::to_owned),
                }
            })
            .collect();
        let watcher = certbot.watcher.as_ref().map(|watcher| {
            let status = watcher.status();
            let health = if !status.running {
                CertbotWatcherHealth::Stopped
            } else if status.degraded {
                CertbotWatcherHealth::Degraded
            } else {
                CertbotWatcherHealth::Healthy
            };
            CertbotWatcherSnapshot {
                health,
                coalesced_events: status.coalesced_events,
                ignored_access_events: status.ignored_access_events,
                backend_errors: status.backend_errors,
                watch_recoveries: status.watch_recoveries,
                watch_refreshes: status.watch_refreshes,
                rescans: status.rescans,
                periodic_rescans: status.periodic_rescans,
                reconciliation_failures: status.reconciliation_failures,
            }
        });
        Ok((certificates, watcher))
    }

    fn direct_file_snapshots(
        &self,
    ) -> Result<
        (
            Vec<DirectFileCertificateSnapshot>,
            Option<DirectFileWatcherSnapshot>,
        ),
        MetricsError,
    > {
        let direct_files = self
            .inner
            .direct_files
            .read()
            .map_err(|_| MetricsError::StatePoisoned("direct-file monitoring"))?;
        let certificates = direct_files
            .reconcilers
            .iter()
            .map(|reconciler| {
                let status = reconciler.status();
                DirectFileCertificateSnapshot {
                    name: status.certificate,
                    active_content_revision: status.active_content_revision,
                    expires_at: status.not_after,
                    last_outcome: status.last_outcome.map(str::to_owned),
                    last_error_code: status.last_error_code.map(str::to_owned),
                }
            })
            .collect();
        let watcher = direct_files.watcher.as_ref().map(|watcher| {
            let status = watcher.status();
            let health = if !status.running {
                CertbotWatcherHealth::Stopped
            } else if status.degraded {
                CertbotWatcherHealth::Degraded
            } else {
                CertbotWatcherHealth::Healthy
            };
            DirectFileWatcherSnapshot {
                health,
                coalesced_events: status.coalesced_events,
                ignored_access_events: status.ignored_access_events,
                backend_errors: status.backend_errors,
                watch_recoveries: status.watch_recoveries,
                watch_refreshes: status.watch_refreshes,
                rescans: status.rescans,
                periodic_rescans: status.periodic_rescans,
                reconciliation_failures: status.reconciliation_failures,
            }
        });
        Ok((certificates, watcher))
    }

    fn acme_managed_snapshots(&self) -> Result<Vec<AcmeManagedCertificateSnapshot>, MetricsError> {
        let managed = self
            .inner
            .acme_managed
            .read()
            .map_err(|_| MetricsError::StatePoisoned("managed ACME monitoring"))?;
        Ok(managed
            .reconcilers
            .iter()
            .map(|reconciler| {
                let status = reconciler.status();
                AcmeManagedCertificateSnapshot {
                    name: status.certificate,
                    directory_url: status.directory_url,
                    disk_revision: status.disk_revision,
                    active_revision: status.active_revision,
                    expires_at: status.not_after,
                    not_before_unix_seconds: status.not_before_unix_seconds,
                    not_after_unix_seconds: status.not_after_unix_seconds,
                    next_action_unix_seconds: status.next_action_unix_seconds,
                    last_outcome: status.last_outcome.map(str::to_owned),
                    last_error_code: status.last_error_code,
                    renewal_information_status: status.renewal_information_status.into(),
                    dns_provider: status.dns_provider,
                    dns_provider_deployment: status.dns_provider_deployment.map(str::to_owned),
                    dns_provider_health: status.dns_provider_health.map(str::to_owned),
                    dns_cleanup_status: status.dns_cleanup_status.into(),
                }
            })
            .collect())
    }

    fn counter_snapshots(&self) -> Result<(TrafficSnapshot, Vec<ListenerSnapshot>), MetricsError> {
        let snapshots = self.listener_snapshots(true)?;

        let mut traffic = TrafficSnapshot::default();
        for snapshot in &snapshots {
            add_total(
                &mut traffic.accepted_connections,
                snapshot.accepted_connections,
                "traffic.acceptedConnections",
            )?;
            add_total(
                &mut traffic.rejected_connections,
                snapshot.rejected_connections,
                "traffic.rejectedConnections",
            )?;
            add_total(
                &mut traffic.active_connections,
                snapshot.active_connections,
                "traffic.activeConnections",
            )?;
            add_total(
                &mut traffic.bytes_received,
                snapshot.bytes_received,
                "traffic.bytesReceived",
            )?;
            add_total(
                &mut traffic.bytes_sent,
                snapshot.bytes_sent,
                "traffic.bytesSent",
            )?;
        }
        Ok((traffic, snapshots))
    }

    fn listener_snapshots(&self, public_only: bool) -> Result<Vec<ListenerSnapshot>, MetricsError> {
        let listeners = self
            .inner
            .listeners
            .read()
            .map_err(|_| MetricsError::StatePoisoned("listeners"))?;
        let mut snapshots = Vec::with_capacity(listeners.order.len());
        for id in &listeners.order {
            let state = listeners
                .states
                .get(id)
                .expect("listener order references registered state");
            if !public_only || state.metric_policy == ListenerMetricPolicy::Public {
                snapshots.push(state.snapshot());
            }
        }
        if public_only {
            snapshots.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        }
        Ok(snapshots)
    }
}
