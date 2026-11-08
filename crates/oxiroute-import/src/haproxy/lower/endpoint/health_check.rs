#[allow(clippy::wildcard_imports)]
use super::*;

impl Lowerer<'_> {
    #[expect(
        clippy::too_many_lines,
        clippy::option_option,
        reason = "one health transaction distinguishes invalid policy from a disabled check"
    )]
    pub(super) fn lower_health_check(
        &mut self,
        section: &EffectiveSection,
        settings: &ProxySettings,
        servers: &[EffectiveServer],
    ) -> Option<Option<HealthCheck>> {
        let checked = servers
            .iter()
            .map(|server| server.check.as_ref().is_some_and(|check| check.value))
            .collect::<HashSet<_>>();
        if checked.len() > 1 {
            self.block_section(
                section,
                "canonical pool health checks cannot target only part of a HAProxy backend",
            );
            return None;
        }
        if checked == HashSet::from([false]) {
            return Some(None);
        }
        for server in servers {
            for (value, description) in [
                (server.interval.as_ref(), "HAProxy health interval"),
                (
                    server.fast_interval.as_ref(),
                    "HAProxy fast health interval",
                ),
                (
                    server.down_interval.as_ref(),
                    "HAProxy down health interval",
                ),
            ] {
                let Some(value) = value else {
                    continue;
                };
                if duration_milliseconds(value.value).is_none() {
                    self.duration_ms(value, description);
                    return None;
                }
            }
        }
        let Some(interval_ms) = Self::common_server_u64(servers, |server| {
            server
                .interval
                .as_ref()
                .and_then(|value| Self::duration_value_ms(value.value))
                .or(Some(2_000))
        }) else {
            self.block_section(
                section,
                "HAProxy servers have non-uniform effective health intervals",
            );
            return None;
        };
        let Ok(fast_interval_ms) = Self::common_server_optional_u64(servers, |server| {
            server
                .fast_interval
                .as_ref()
                .and_then(|value| Self::duration_value_ms(value.value))
        }) else {
            self.block_section(
                section,
                "HAProxy servers have non-uniform effective fast health intervals",
            );
            return None;
        };
        let Ok(down_interval_ms) = Self::common_server_optional_u64(servers, |server| {
            server
                .down_interval
                .as_ref()
                .and_then(|value| Self::duration_value_ms(value.value))
        }) else {
            self.block_section(
                section,
                "HAProxy servers have non-uniform effective down health intervals",
            );
            return None;
        };
        let Some(healthy_threshold) = Self::common_server_u16(servers, |server| {
            server
                .rise
                .as_ref()
                .and_then(|value| u16::try_from(value.value).ok())
                .or(Some(2))
        }) else {
            self.block_section(
                section,
                "HAProxy servers have non-uniform effective health rise thresholds",
            );
            return None;
        };
        let Some(unhealthy_threshold) = Self::common_server_u16(servers, |server| {
            server
                .fall
                .as_ref()
                .and_then(|value| u16::try_from(value.value).ok())
                .or(Some(3))
        }) else {
            self.block_section(
                section,
                "HAProxy servers have non-uniform effective health fall thresholds",
            );
            return None;
        };
        let timeout_ms = interval_ms.max(1);
        let request = match (
            settings.http_check.as_ref().map(|value| &value.value),
            settings.http_check_send.as_ref(),
        ) {
            (Some(OptionState::Disabled), Some(send)) => {
                self.block_provenance(
                    &send.provenance,
                    "HAProxy HTTP health-check send cannot be combined with disabled option httpchk",
                );
                return None;
            }
            (_, Some(send)) => Some((&send.value, &send.provenance)),
            (Some(OptionState::Enabled(check)), None) => settings
                .http_check
                .as_ref()
                .map(|value| (check, &value.provenance)),
            _ => None,
        };
        let (kind, path, host, http_version) = match request {
            Some((check, provenance)) => {
                if check.method != b"GET" {
                    self.block_provenance(
                        provenance,
                        "HAProxy HTTP health check method is not representable by the canonical GET health check",
                    );
                    return None;
                }
                let (Ok(path), Ok(host)) = (
                    std::str::from_utf8(&check.uri),
                    check.host.as_deref().map(std::str::from_utf8).transpose(),
                ) else {
                    self.block_provenance(
                        provenance,
                        "HAProxy HTTP check path or host is not UTF-8",
                    );
                    return None;
                };
                let version = match check.version.as_slice() {
                    b"HTTP/1.0" => Some(oxiroute_config::HealthHttpVersion::Http10),
                    b"HTTP/1.1" => Some(oxiroute_config::HealthHttpVersion::Http11),
                    _ => None,
                };
                (
                    HealthCheckType::Http,
                    Some(path.into()),
                    host.map(str::to_owned),
                    version,
                )
            }
            None => (HealthCheckType::Tcp, None, None, None),
        };
        let expected_status = match (kind, settings.http_check_expect.as_ref()) {
            (HealthCheckType::Http, Some(expect)) => match expect.value.as_slice() {
                [range] if range.start == range.end => Some(range.start),
                _ => {
                    self.block_value(
                        expect,
                        "HAProxy HTTP health status ranges cannot lower to one exact canonical status",
                    );
                    return None;
                }
            },
            (HealthCheckType::Http, None) => {
                self.block_section(
                    section,
                    "HAProxy HTTP health checks require an exact http-check expect status before canonical lowering",
                );
                return None;
            }
            (HealthCheckType::Tcp, _) => None,
        };
        Some(Some(HealthCheck {
            kind,
            interval_ms,
            timeout_ms,
            healthy_threshold,
            unhealthy_threshold,
            startup: HealthStartup::Healthy,
            fast_interval_ms,
            down_interval_ms,
            host,
            path,
            expected_status,
            http_version,
        }))
    }

    pub(super) fn record_health_check_provenance(
        &mut self,
        path: &CanonicalPath,
        settings: &ProxySettings,
        servers: &[EffectiveServer],
    ) {
        let request_sources = settings
            .http_check_send
            .as_ref()
            .map(|value| &value.provenance)
            .or_else(|| settings.http_check.as_ref().map(|value| &value.provenance));
        if let Some(provenance) = request_sources {
            let sources = provenance_sources(provenance);
            for field in ["kind", "path", "host", "http_version"] {
                self.record(path.field("health_check").field(field), sources.clone());
            }
        }
        if let Some(expect) = &settings.http_check_expect {
            self.record(
                path.field("health_check").field("expected_status"),
                provenance_sources(&expect.provenance),
            );
        }
        self.record_server_sources(
            path.field("health_check").field("interval_ms"),
            servers,
            |server| server.interval.as_ref(),
        );
        self.record_server_sources(
            path.field("health_check").field("timeout_ms"),
            servers,
            |server| server.interval.as_ref(),
        );
        self.record_server_sources(
            path.field("health_check").field("healthy_threshold"),
            servers,
            |server| server.rise.as_ref(),
        );
        self.record_server_sources(
            path.field("health_check").field("unhealthy_threshold"),
            servers,
            |server| server.fall.as_ref(),
        );
        self.record_server_sources(
            path.field("health_check").field("fast_interval_ms"),
            servers,
            |server| server.fast_interval.as_ref(),
        );
        self.record_server_sources(
            path.field("health_check").field("down_interval_ms"),
            servers,
            |server| server.down_interval.as_ref(),
        );
    }
}
