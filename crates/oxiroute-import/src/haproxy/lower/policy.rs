use std::{collections::HashSet, time::Duration};

use oxiroute_config::{
    HttpProxyPolicy, HttpRequestHeaderMutation, HttpRequestHeaderValue, HttpResponseHeaderMutation,
    HttpRetryPolicy, HttpRetryTarget, HttpRetryTrigger, HttpUpstreamHost, Protocol,
};

use crate::{Diagnostic, DiagnosticStage, E_SEMANTICS_NOT_REPRESENTABLE, ProvenanceSpan, Severity};

use super::{Lowerer, Representability};
use crate::haproxy::{
    EffectiveBind, EffectiveFrontend, EffectiveListen, EffectiveSection, EffectiveValue,
    HttpHeaderValue, HttpRequestRule, HttpResponseRule, OptionState, Provenance, ProxyMode,
    ProxySettings, RetryOn, RetryOnTrigger, SectionId, SemanticBlockerKind,
};

use super::provenance::{deduplicate_sources, extend_sources, provenance_sources};

struct ModeTarget {
    section: SectionId,
    reference: Option<Provenance>,
}

pub(super) struct ModeSelection {
    pub(super) protocol: Protocol,
    pub(super) sources: Vec<ProvenanceSpan>,
}

impl Lowerer<'_> {
    pub(super) fn frontend_mode_selection(
        &mut self,
        frontend: &EffectiveFrontend,
    ) -> Option<ModeSelection> {
        let mut targets = Vec::new();
        if let Some(reference) = &frontend.settings.default_backend {
            targets.push(ModeTarget {
                section: reference.value.target,
                reference: Some(reference.provenance.clone()),
            });
        }
        targets.extend(frontend.use_backends.iter().map(|rule| ModeTarget {
            section: rule.value.backend.target,
            reference: Some(rule.provenance.clone()),
        }));
        self.mode_selection(&frontend.section, "frontend", &frontend.settings, targets)
    }

    pub(super) fn listen_mode_selection(
        &mut self,
        listen: &EffectiveListen,
    ) -> Option<ModeSelection> {
        let mut targets = if let Some(reference) = &listen.settings.default_backend {
            vec![ModeTarget {
                section: reference.value.target,
                reference: Some(reference.provenance.clone()),
            }]
        } else {
            vec![ModeTarget {
                section: listen.section.id,
                reference: None,
            }]
        };
        targets.extend(listen.use_backends.iter().map(|rule| ModeTarget {
            section: rule.value.backend.target,
            reference: Some(rule.provenance.clone()),
        }));
        self.mode_selection(&listen.section, "listen", &listen.settings, targets)
    }

    fn mode_selection(
        &mut self,
        section: &EffectiveSection,
        section_kind: &str,
        settings: &ProxySettings,
        targets: Vec<ModeTarget>,
    ) -> Option<ModeSelection> {
        let Some(frontend_mode) = settings.mode.as_ref() else {
            self.block_section(
                section,
                &format!(
                    "HAProxy {section_kind} mode must be explicit or inherited before canonical lowering"
                ),
            );
            return None;
        };
        let frontend_protocol = self.protocol_for_mode(frontend_mode)?;
        if targets.is_empty() {
            if frontend_protocol == Protocol::Http
                && settings.http_request_rules.iter().any(|rule| {
                    matches!(
                        rule.value,
                        HttpRequestRule::Redirect { .. } | HttpRequestRule::FixedResponse { .. }
                    )
                })
            {
                return Some(ModeSelection {
                    protocol: frontend_protocol,
                    sources: provenance_sources(&frontend_mode.provenance),
                });
            }
            self.block_section(
                section,
                &format!(
                    "HAProxy {section_kind} has no selected backend mode for canonical protocol selection"
                ),
            );
            return None;
        }

        let mut sources = provenance_sources(&frontend_mode.provenance);
        let mut selected_protocol = None;
        let mut seen = HashSet::new();
        for target in targets {
            if !seen.insert(target.section) {
                continue;
            }
            if let Some(reference) = &target.reference {
                extend_sources(&mut sources, reference);
            }
            let backend_mode = self
                .backend_view(target.section)
                .and_then(|backend| backend.settings().mode.clone());
            let Some(backend_mode) = backend_mode else {
                self.block_mode_reference(
                    section,
                    target.reference.as_ref(),
                    "selected HAProxy backend has no effective mode",
                );
                return None;
            };
            let backend_protocol = self.protocol_for_mode(&backend_mode)?;
            extend_sources(&mut sources, &backend_mode.provenance);
            if backend_protocol != frontend_protocol {
                self.block_mode_transition(
                    section,
                    section_kind,
                    frontend_mode,
                    &backend_mode,
                    target.reference.as_ref(),
                );
                return None;
            }
            if selected_protocol.is_some_and(|protocol| protocol != backend_protocol) {
                self.block_mode_reference(
                    section,
                    target.reference.as_ref(),
                    "selected HAProxy backends disagree about proxy mode",
                );
                return None;
            }
            selected_protocol = Some(backend_protocol);
        }
        deduplicate_sources(&mut sources);
        Some(ModeSelection {
            protocol: selected_protocol.expect("mode selection has at least one backend"),
            sources,
        })
    }

    fn protocol_for_mode(&mut self, mode: &EffectiveValue<ProxyMode>) -> Option<Protocol> {
        match &mode.value {
            ProxyMode::Http => Some(Protocol::Http),
            ProxyMode::Tcp => Some(Protocol::Tcp),
            ProxyMode::Unsupported(_) => {
                self.block_value(
                    mode,
                    "unsupported HAProxy mode cannot be lowered as an HTTP or TCP listener",
                );
                None
            }
        }
    }

    fn block_mode_transition(
        &mut self,
        section: &EffectiveSection,
        section_kind: &str,
        frontend: &EffectiveValue<ProxyMode>,
        backend: &EffectiveValue<ProxyMode>,
        reference: Option<&Provenance>,
    ) {
        let mut diagnostic = Diagnostic::new(
            E_SEMANTICS_NOT_REPRESENTABLE,
            Severity::Error,
            DiagnosticStage::Lower,
            format!(
                "HAProxy {section_kind} {} mode transitions to an {} backend, which canonical service semantics cannot represent",
                proxy_mode_name(&frontend.value),
                proxy_mode_name(&backend.value)
            ),
        )
        .with_primary_span(reference.map_or(section.span, |provenance| provenance.origin_span))
        .with_related_span(
            frontend.provenance.origin_span,
            "frontend/listen mode is defined here",
        )
        .with_related_span(
            backend.provenance.origin_span,
            "selected backend mode is defined here",
        );
        if let Some(reference) = reference {
            for source in provenance_sources(reference).into_iter().skip(1) {
                diagnostic =
                    diagnostic.with_related_span(source.span, "selected backend reference");
            }
        }
        self.diagnostics.push(diagnostic);
    }

    fn block_mode_reference(
        &mut self,
        section: &EffectiveSection,
        reference: Option<&Provenance>,
        message: &str,
    ) {
        if let Some(reference) = reference {
            self.block_provenance(reference, message);
        } else {
            self.block_section(section, message);
        }
    }

    pub(super) fn listener_caps(
        &mut self,
        section: &EffectiveSection,
        binds: &[EffectiveBind],
        maxconn: Option<&EffectiveValue<u64>>,
    ) -> Option<Vec<(Option<u64>, Vec<ProvenanceSpan>)>> {
        let local = maxconn.filter(|maxconn| maxconn.value != 0);
        for bind in binds {
            if let Some(zero) = bind.maxconn.as_ref().filter(|maxconn| maxconn.value == 0) {
                self.block_value(
                    zero,
                    "HAProxy bind maxconn zero has no documented unlimited-listener meaning",
                );
                return None;
            }
        }
        if binds.len() == 1 {
            let bind = &binds[0];
            let bind_cap = bind.maxconn.as_ref().filter(|maxconn| maxconn.value != 0);
            let mut candidates = Vec::new();
            if let Some(local) = local {
                candidates.push((local.value, provenance_sources(&local.provenance)));
            }
            if let Some(bind_cap) = bind_cap {
                candidates.push((bind_cap.value, provenance_sources(&bind_cap.provenance)));
            }
            let value = candidates.iter().map(|(value, _)| *value).min();
            let mut sources = Vec::new();
            for (_, candidate_sources) in candidates {
                sources.extend(candidate_sources);
            }
            if let Some(zero) = maxconn.filter(|maxconn| maxconn.value == 0) {
                extend_sources(&mut sources, &zero.provenance);
            }
            if let Some(zero) = bind.maxconn.as_ref().filter(|maxconn| maxconn.value == 0) {
                extend_sources(&mut sources, &zero.provenance);
            }
            deduplicate_sources(&mut sources);
            return Some(vec![(value, sources)]);
        }

        let Some(local) = local else {
            let zero_sources =
                maxconn.map_or_else(Vec::new, |maxconn| provenance_sources(&maxconn.provenance));
            return Some(
                binds
                    .iter()
                    .map(|bind| {
                        bind.maxconn.as_ref().map_or_else(
                            || (None, zero_sources.clone()),
                            |cap| (Some(cap.value), provenance_sources(&cap.provenance)),
                        )
                    })
                    .collect(),
            );
        };
        let caps = binds
            .iter()
            .map(|bind| bind.maxconn.as_ref().filter(|maxconn| maxconn.value != 0))
            .collect::<Option<Vec<_>>>();
        let Some(caps) = caps else {
            if let Some(maxconn) = maxconn {
                self.block_value(
                    maxconn,
                    "HAProxy proxy maxconn is aggregate across binds without exact per-socket caps",
                );
            } else {
                self.block_section(
                    section,
                    "HAProxy multiple binds require explicit per-socket maxconn values",
                );
            }
            return None;
        };
        let sum = caps
            .iter()
            .try_fold(0_u64, |sum, cap| sum.checked_add(cap.value));
        if sum.is_none_or(|sum| sum > local.value) {
            self.block_value(
                local,
                "HAProxy proxy maxconn remains an active aggregate across multiple binds",
            );
            return None;
        }
        Some(
            caps.into_iter()
                .map(|cap| (Some(cap.value), provenance_sources(&cap.provenance)))
                .collect(),
        )
    }

    pub(super) fn lower_http_policy(
        &mut self,
        section: &EffectiveSection,
        frontend: &ProxySettings,
        targets: &HashSet<SectionId>,
    ) -> Option<(u64, u64, Vec<ProvenanceSpan>)> {
        let mut decision = Representability::new(true);
        let mut timeouts = None;
        let mut sources = Vec::new();
        for target in targets {
            let Some(backend) = self.backend_view(*target) else {
                decision.require(false);
                continue;
            };
            let backend_section = backend.section().clone();
            let settings = backend.settings().clone();
            let candidate = self.http_timeouts(&backend_section, &settings);
            match (timeouts, candidate) {
                (None, Some(value)) => timeouts = Some(value),
                (Some(first), Some(value)) if first == value => {}
                (Some(_), Some(_)) => {
                    self.block_section(
                        section,
                        "HAProxy routes use different backend timeout policies, but canonical HTTP timeouts are per service",
                    );
                    decision.require(false);
                }
                _ => decision.require(false),
            }
            for value in [
                settings.timeouts.connect.as_ref(),
                settings.timeouts.server.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                extend_sources(&mut sources, &value.provenance);
            }
            if let Some(retries) = &settings.retries {
                extend_sources(&mut sources, &retries.provenance);
            }
            if let Some(retry_on) = &settings.retry_on {
                extend_sources(&mut sources, &retry_on.provenance);
            }
            for rule in &settings.http_request_rules {
                extend_sources(&mut sources, &rule.provenance);
            }
            for rule in &settings.http_response_rules {
                extend_sources(&mut sources, &rule.provenance);
            }
        }
        for rule in &frontend.http_request_rules {
            extend_sources(&mut sources, &rule.provenance);
        }
        for rule in &frontend.http_response_rules {
            extend_sources(&mut sources, &rule.provenance);
        }
        if targets.is_empty() {
            self.block_section(
                section,
                "HAProxy HTTP proxy has no complete route target; no fallback route will be inserted",
            );
            decision.require(false);
        }
        if !decision.is_complete() {
            return None;
        }
        let (connect_timeout_ms, server_timeout_ms) = timeouts?;
        Some((connect_timeout_ms, server_timeout_ms, sources))
    }

    pub(super) fn lower_proxy_policies(
        &mut self,
        frontend: &ProxySettings,
        targets: &HashSet<SectionId>,
    ) -> Option<std::collections::HashMap<SectionId, HttpProxyPolicy>> {
        let mut policies = std::collections::HashMap::new();
        for target in targets {
            let backend = self.backend_view(*target)?;
            let section = backend.section().clone();
            let settings = backend.settings().clone();
            let mut request_headers = self.lower_forward_headers(frontend, &settings)?;
            let (upstream_host, mut explicit_request_headers) =
                self.lower_request_header_rules(frontend, &settings)?;
            request_headers.append(&mut explicit_request_headers);
            let response_headers = self.lower_response_header_rules(frontend, &settings)?;
            let (max_retries, final_redispatch, delay_ms) =
                self.http_retries(&section, &settings)?;
            let (triggers, response_statuses) = Self::lower_retry_on(&settings);
            policies.insert(
                *target,
                HttpProxyPolicy {
                    upstream_host,
                    request_headers,
                    response_headers,
                    retry: HttpRetryPolicy {
                        max_retries,
                        target: HttpRetryTarget::SameServer,
                        delay_ms,
                        final_redispatch,
                        triggers,
                        response_statuses,
                        ..HttpRetryPolicy::default()
                    },
                    ..HttpProxyPolicy::default()
                },
            );
        }
        Some(policies)
    }

    fn lower_request_header_rules(
        &mut self,
        frontend: &ProxySettings,
        backend: &ProxySettings,
    ) -> Option<(HttpUpstreamHost, Vec<HttpRequestHeaderMutation>)> {
        let mut upstream_host = HttpUpstreamHost::PreserveIncoming;
        let mut host_set = false;
        let mut mutations = Vec::new();
        for rule in frontend
            .http_request_rules
            .iter()
            .chain(&backend.http_request_rules)
        {
            match &rule.value {
                HttpRequestRule::SetHeader { name, value }
                    if name.eq_ignore_ascii_case(b"host") =>
                {
                    if host_set {
                        self.block_value(
                            rule,
                            "HAProxy sets the upstream Host header more than once",
                        );
                        return None;
                    }
                    upstream_host = match value {
                        HttpHeaderValue::Literal(value) => {
                            let Ok(value) = std::str::from_utf8(value) else {
                                self.block_value(
                                    rule,
                                    "HAProxy Host literal is not canonical UTF-8 authority text",
                                );
                                return None;
                            };
                            HttpUpstreamHost::Literal {
                                value: value.into(),
                            }
                        }
                        HttpHeaderValue::IncomingAuthority => HttpUpstreamHost::PreserveIncoming,
                        HttpHeaderValue::ClientIp => {
                            self.block_value(
                                rule,
                                "HAProxy client IP cannot form an upstream Host authority",
                            );
                            return None;
                        }
                    };
                    host_set = true;
                }
                HttpRequestRule::RemoveHeader { name } if name.eq_ignore_ascii_case(b"host") => {
                    self.block_value(
                        rule,
                        "HAProxy removal of the required upstream Host header is not canonical",
                    );
                    return None;
                }
                HttpRequestRule::SetHeader { name, value } => {
                    let Ok(name) = std::str::from_utf8(name) else {
                        self.block_value(rule, "HAProxy request-header name is not UTF-8");
                        return None;
                    };
                    let value = match value {
                        HttpHeaderValue::Literal(value) => {
                            let Ok(value) = std::str::from_utf8(value) else {
                                self.block_value(
                                    rule,
                                    "HAProxy request-header literal is not UTF-8",
                                );
                                return None;
                            };
                            HttpRequestHeaderValue::Literal {
                                value: value.into(),
                            }
                        }
                        HttpHeaderValue::ClientIp => HttpRequestHeaderValue::ClientIp,
                        HttpHeaderValue::IncomingAuthority => {
                            HttpRequestHeaderValue::IncomingAuthority
                        }
                    };
                    mutations.push(HttpRequestHeaderMutation::Set {
                        name: name.into(),
                        value,
                    });
                }
                HttpRequestRule::RemoveHeader { name } => {
                    let Ok(name) = std::str::from_utf8(name) else {
                        self.block_value(rule, "HAProxy request-header name is not UTF-8");
                        return None;
                    };
                    mutations.push(HttpRequestHeaderMutation::Remove { name: name.into() });
                }
                HttpRequestRule::Redirect { .. } | HttpRequestRule::FixedResponse { .. } => {}
            }
        }
        Some((upstream_host, mutations))
    }

    fn lower_response_header_rules(
        &mut self,
        frontend: &ProxySettings,
        backend: &ProxySettings,
    ) -> Option<Vec<HttpResponseHeaderMutation>> {
        let mut mutations = Vec::new();
        for rule in frontend
            .http_response_rules
            .iter()
            .chain(&backend.http_response_rules)
        {
            match &rule.value {
                HttpResponseRule::SetHeader { name, value } => {
                    let (Ok(name), Ok(value)) =
                        (std::str::from_utf8(name), std::str::from_utf8(value))
                    else {
                        self.block_value(rule, "HAProxy response-header mutation is not UTF-8");
                        return None;
                    };
                    mutations.push(HttpResponseHeaderMutation::Set {
                        name: name.into(),
                        value: value.into(),
                        always: true,
                    });
                }
                HttpResponseRule::RemoveHeader { name } => {
                    let Ok(name) = std::str::from_utf8(name) else {
                        self.block_value(rule, "HAProxy response-header name is not UTF-8");
                        return None;
                    };
                    mutations.push(HttpResponseHeaderMutation::Remove { name: name.into() });
                }
            }
        }
        Some(mutations)
    }

    fn lower_forward_headers(
        &mut self,
        frontend: &ProxySettings,
        backend: &ProxySettings,
    ) -> Option<Vec<HttpRequestHeaderMutation>> {
        let mut enabled = [frontend.forward_for.as_ref(), backend.forward_for.as_ref()]
            .into_iter()
            .flatten()
            .filter(|value| matches!(&value.value, OptionState::Enabled(_)))
            .collect::<Vec<_>>();
        if enabled.len() == 2 && enabled[0].provenance.origin == enabled[1].provenance.origin {
            enabled.truncate(1);
        }
        if enabled.is_empty() {
            return Some(Vec::new());
        }
        if enabled.len() != 1 {
            self.block_value(
                enabled[1],
                "multiple effective HAProxy forwardfor policies would append the client address more than once",
            );
            return None;
        }
        let policy = enabled[0];
        let OptionState::Enabled(forward_for) = &policy.value else {
            unreachable!("enabled forwardfor policy was filtered")
        };
        if forward_for.if_none {
            self.block_value(
                policy,
                "HAProxy forwardfor if-none condition has no canonical header-presence policy",
            );
            return None;
        }
        if forward_for.header.is_some() {
            self.block_value(
                policy,
                "HAProxy forwardfor custom header names are not canonical X-Forwarded-For",
            );
            return None;
        }
        let except_source_cidrs = match forward_for.except.as_deref() {
            None => Vec::new(),
            Some(b"127.0.0.0/8") => vec!["127.0.0.0/8".into()],
            Some(_) => {
                self.block_value(
                    policy,
                    "HAProxy forwardfor except is outside the canonical source-CIDR subset",
                );
                return None;
            }
        };
        Some(vec![HttpRequestHeaderMutation::Set {
            name: "x-forwarded-for".into(),
            value: HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes: 8_192,
                except_source_cidrs,
            },
        }])
    }

    pub(super) fn lower_tcp_policy(
        &mut self,
        section: &EffectiveSection,
        frontend: &ProxySettings,
        backend: &ProxySettings,
    ) -> Option<(u64, u64)> {
        let mut decision = Representability::new(true);
        decision.require(!self.block_forward_for(frontend));
        decision.require(!self.block_forward_for(backend));
        decision.require(!self.block_redispatch(backend));
        if let Some(retry_on) = &backend.retry_on {
            self.block_value(
                retry_on,
                "HAProxy retry-on has no canonical TCP retry policy equivalent",
            );
            decision.require(false);
        }
        decision.require(self.require_zero_retries(section, backend));
        for timeout in [
            frontend.timeouts.http_request.as_ref(),
            frontend.timeouts.http_keep_alive.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            self.block_value(
                timeout,
                "HAProxy HTTP timeout classes have no meaning in a canonical TCP service",
            );
            decision.require(false);
        }

        let connect = backend
            .timeouts
            .connect
            .as_ref()
            .and_then(|value| self.duration_ms(value, "HAProxy connect timeout"));
        let client = frontend
            .timeouts
            .client
            .as_ref()
            .and_then(|value| self.duration_ms(value, "HAProxy client timeout"));
        let server = backend
            .timeouts
            .server
            .as_ref()
            .and_then(|value| self.duration_ms(value, "HAProxy server timeout"));
        if connect.is_none() {
            self.block_section(
                section,
                "HAProxy timeout connect must be explicit for canonical TCP lowering",
            );
            decision.require(false);
        }
        let idle = match (client, server) {
            (Some(client), Some(server)) if client == server => Some(client),
            (Some(_), Some(_)) => {
                self.block_section(
                    section,
                    "separate HAProxy client/server timeout scopes cannot map to one canonical TCP idle timeout",
                );
                decision.require(false);
                None
            }
            _ => {
                self.block_section(
                    section,
                    "HAProxy timeout client and timeout server must both be explicit for canonical TCP lowering",
                );
                decision.require(false);
                None
            }
        };
        decision.is_complete().then(|| {
            (
                connect.expect("complete connect timeout"),
                idle.expect("complete idle timeout"),
            )
        })
    }

    pub(super) fn http_timeouts(
        &mut self,
        section: &EffectiveSection,
        settings: &ProxySettings,
    ) -> Option<(u64, u64)> {
        let connect = settings
            .timeouts
            .connect
            .as_ref()
            .and_then(|value| self.duration_ms(value, "HAProxy connect timeout"));
        let server = settings
            .timeouts
            .server
            .as_ref()
            .and_then(|value| self.duration_ms(value, "HAProxy server timeout"));
        if let (Some(connect), Some(server)) = (connect, server) {
            Some((connect, server))
        } else {
            self.block_section(
                section,
                "HAProxy timeout connect and timeout server must both be explicit for canonical HTTP lowering",
            );
            None
        }
    }

    fn http_retries(
        &mut self,
        _section: &EffectiveSection,
        settings: &ProxySettings,
    ) -> Option<(u8, bool, u64)> {
        let redispatch = match settings.redispatch.as_ref().map(|value| &value.value) {
            Some(OptionState::Enabled(redispatch)) if redispatch.interval.is_none() => true,
            Some(OptionState::Enabled(_)) => {
                self.block_value(
                    settings.redispatch.as_ref().expect("enabled redispatch"),
                    "HAProxy redispatch interval forms are outside the supported final-attempt subset",
                );
                return None;
            }
            Some(OptionState::Disabled) | None => false,
        };
        let configured_max_retries = if let Some(retries) = &settings.retries {
            match u8::try_from(retries.value) {
                Ok(value) if value <= 3 => value,
                _ => {
                    self.block_value(retries, "HAProxy retries exceed the canonical retry bound");
                    return None;
                }
            }
        } else {
            3
        };
        let max_retries = if matches!(
            settings.retry_on.as_ref().map(|value| &value.value),
            Some(RetryOn::None)
        ) {
            0
        } else {
            configured_max_retries
        };
        let delay_ms = settings
            .timeouts
            .connect
            .as_ref()
            .and_then(|timeout| crate::canonical::duration_milliseconds(timeout.value))
            .unwrap_or(1_000)
            .min(1_000);
        Some((max_retries, redispatch && max_retries > 0, delay_ms))
    }

    fn lower_retry_on(settings: &ProxySettings) -> (Vec<HttpRetryTrigger>, Vec<u16>) {
        match settings.retry_on.as_ref().map(|value| &value.value) {
            None => (
                vec![
                    HttpRetryTrigger::ConnectFailure,
                    HttpRetryTrigger::ConnectTimeout,
                ],
                Vec::new(),
            ),
            Some(RetryOn::None) => (Vec::new(), Vec::new()),
            Some(RetryOn::Rules {
                triggers,
                response_statuses,
            }) => {
                let mut lowered = Vec::new();
                for trigger in triggers {
                    match trigger {
                        RetryOnTrigger::ConnFailure => {
                            if !lowered.contains(&HttpRetryTrigger::ConnectFailure) {
                                lowered.extend([
                                    HttpRetryTrigger::ConnectFailure,
                                    HttpRetryTrigger::ConnectTimeout,
                                ]);
                            }
                        }
                        RetryOnTrigger::EmptyResponse => {
                            lowered.push(HttpRetryTrigger::EmptyResponse);
                        }
                        RetryOnTrigger::ResponseTimeout => {
                            lowered.push(HttpRetryTrigger::ResponseTimeout);
                        }
                        RetryOnTrigger::JunkResponse => {
                            lowered.push(HttpRetryTrigger::JunkResponse);
                        }
                    }
                }
                (lowered, response_statuses.clone())
            }
        }
    }

    pub(super) fn require_zero_retries(
        &mut self,
        section: &EffectiveSection,
        settings: &ProxySettings,
    ) -> bool {
        match &settings.retries {
            Some(retries) if retries.value == 0 => true,
            Some(retries) => {
                self.block_value(
                    retries,
                    "HAProxy retries can repeat or redispatch broader traffic than canonical safe distinct-endpoint retries",
                );
                false
            }
            None => {
                self.block_section(
                    section,
                    "HAProxy retries must be explicitly zero because its native default is not canonical retry behavior",
                );
                false
            }
        }
    }

    pub(super) fn block_redispatch(&mut self, settings: &ProxySettings) -> bool {
        if let Some(redispatch) = settings
            .redispatch
            .as_ref()
            .filter(|value| matches!(value.value, OptionState::Enabled(_)))
        {
            self.block_value(
                redispatch,
                "HAProxy redispatch persistence and retry timing have no canonical retry equivalent",
            );
            true
        } else {
            false
        }
    }

    pub(super) fn block_forward_for(&mut self, settings: &ProxySettings) -> bool {
        if let Some(forward_for) = settings
            .forward_for
            .as_ref()
            .filter(|value| matches!(&value.value, OptionState::Enabled(_)))
        {
            self.block_value(
                forward_for,
                "HAProxy forwardfor header insertion can append a duplicate field and is not one canonical set-header mutation",
            );
            true
        } else {
            false
        }
    }

    pub(super) fn block_semantic_settings(&mut self, settings: &ProxySettings) -> bool {
        for blocker in &settings.semantic_blockers {
            self.block_value(blocker, semantic_blocker_message(blocker.value.kind));
        }
        settings
            .semantic_blockers
            .iter()
            .any(|blocker| blocker.value.kind != SemanticBlockerKind::Logging)
    }

    pub(super) fn duration_ms(
        &mut self,
        value: &EffectiveValue<Duration>,
        description: &str,
    ) -> Option<u64> {
        let exact = crate::canonical::duration_milliseconds(value.value);
        if exact.is_none() {
            self.block_provenance(
                &value.provenance,
                &format!("{description} is not exactly representable in canonical milliseconds"),
            );
        }
        exact
    }
}

fn proxy_mode_name(mode: &ProxyMode) -> &'static str {
    match mode {
        ProxyMode::Http => "HTTP",
        ProxyMode::Tcp => "TCP",
        ProxyMode::Unsupported(_) => "unsupported",
    }
}

pub(super) const fn semantic_blocker_message(kind: SemanticBlockerKind) -> &'static str {
    match kind {
        SemanticBlockerKind::ConflictingDirective => {
            "conflicting HAProxy directives do not define one lowerable effective value"
        }
        SemanticBlockerKind::GlobalSecurity => {
            "HAProxy global TLS or security policy has no canonical equivalent"
        }
        SemanticBlockerKind::Logging => {
            "HAProxy logging behavior is not represented by canonical services"
        }
        SemanticBlockerKind::Mode => {
            "unsupported HAProxy mode cannot be lowered as an HTTP or TCP listener"
        }
        SemanticBlockerKind::ProxyDefault => {
            "HAProxy proxy default changes behavior that is not represented canonically"
        }
        SemanticBlockerKind::Retry => {
            "HAProxy retry-on form is not represented by supported canonical retry policy"
        }
        SemanticBlockerKind::Timeout => "HAProxy timeout class has no canonical timeout scope",
        SemanticBlockerKind::Tls => {
            "HAProxy TLS policy or certificate selection is not exactly representable"
        }
    }
}
