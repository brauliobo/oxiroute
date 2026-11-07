#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn rtmp_applications(service: &oxiroute_config::RtmpService) -> Value {
    Value::Array(
        service
            .applications
            .iter()
            .map(|application| {
                let manual_recorders = application
                    .recorders
                    .iter()
                    .filter(|recorder| recorder.start == RtmpRecorderStart::Manual)
                    .count();
                json!({
                    "name": application.name,
                    "live": application.live,
                    "idleStreams": application.idle_streams,
                    "pushTargetCount": application.push_targets.len(),
                    "fanout": {
                        "maxSubscribers": application.fanout.max_subscribers,
                        "maxQueueMessagesPerSubscriber": application.fanout.max_queue_messages_per_subscriber,
                        "maxQueueBytesPerSubscriber": application.fanout.max_queue_bytes_per_subscriber,
                    },
                    "recording": {
                        "supported": !application.recorders.is_empty(),
                        "recorderCount": application.recorders.len(),
                        "manualRecorderCount": manual_recorders,
                        "continuousRecorderCount": application.recorders.len() - manual_recorders,
                    },
                })
            })
            .collect(),
    )
}

pub(super) fn host_selector(selector: &HttpHostSelector) -> Value {
    match selector {
        HttpHostSelector::NormalizedHost { value } => json!({
            "kind": "normalized_host",
            "value": value,
        }),
        HttpHostSelector::ExactAuthority { value } => json!({
            "kind": "exact_authority",
            "value": value,
        }),
        HttpHostSelector::AsciiCaseInsensitiveExactAuthority { value } => json!({
            "kind": "ascii_case_insensitive_exact_authority",
            "value": value,
        }),
        HttpHostSelector::NginxLeadingWildcard { value } => json!({
            "kind": "nginx_leading_wildcard",
            "value": value,
        }),
        HttpHostSelector::NginxLeadingDot { value } => json!({
            "kind": "nginx_leading_dot",
            "value": value,
        }),
    }
}

pub(super) fn path_selector(selector: &HttpPathSelector) -> Value {
    match selector {
        HttpPathSelector::SegmentPrefix { value } => json!({
            "kind": "segment_prefix",
            "value": value,
        }),
        HttpPathSelector::RawPrefix { value } => json!({
            "kind": "raw_prefix",
            "value": value,
        }),
        HttpPathSelector::Exact { value } => json!({
            "kind": "exact",
            "value": value,
        }),
        HttpPathSelector::AsciiCaseInsensitiveExact { value } => json!({
            "kind": "ascii_case_insensitive_exact",
            "value": value,
        }),
    }
}

pub(super) fn access_policy(policy: &HttpAccessPolicy) -> Value {
    match policy {
        HttpAccessPolicy::BearerTokenFile {
            token_file_path: _,
            header_name,
            realm,
        } => json!({
            "type": "bearer_token_file",
            "headerName": header_name,
            "realm": realm,
        }),
        HttpAccessPolicy::BasicHtpasswdFile {
            htpasswd_file_path: _,
            realm,
        } => json!({
            "type": "basic_htpasswd_file",
            "realm": realm,
        }),
    }
}

pub(super) fn route_action(action: &HttpRouteAction) -> Value {
    match action {
        HttpRouteAction::Proxy {
            upstream_pool,
            policy,
        } => json!({
            "type": "proxy",
            "upstreamPool": upstream_pool,
            "upstreamHost": match &policy.upstream_host {
                HttpUpstreamHost::PreserveIncoming => "preserve_incoming",
                HttpUpstreamHost::NginxHost { .. } => "nginx_host",
                HttpUpstreamHost::Endpoint { .. } => "endpoint",
                HttpUpstreamHost::Literal { .. } => "literal",
            },
            "requestHeaderMutationCount": policy.request_headers.len(),
            "responseHeaderMutationCount": policy.response_headers.len(),
            "cookiePathRewriteCount": policy.response_cookie_path_rewrites.len(),
            "cookieAttributePolicyCount": policy.response_cookie_attributes.len(),
            "retry": {
                "maxRetries": policy.retry.max_retries,
                "triggers": policy.retry.triggers,
                "target": policy.retry.target,
                "delayMs": policy.retry.delay_ms,
                "finalRedispatch": policy.retry.final_redispatch,
            },
        }),
        HttpRouteAction::FixedResponse {
            status,
            body,
            headers,
        } => json!({
            "type": "fixed_response",
            "status": status,
            "bodyBytes": body.len(),
            "headerCount": headers.len(),
        }),
        HttpRouteAction::Redirect {
            status,
            location,
            headers,
        } => json!({
            "type": "redirect",
            "status": status,
            "locationType": match location {
                oxiroute_config::HttpRedirectLocation::Literal { .. } => "literal",
                oxiroute_config::HttpRedirectLocation::RequestTemplate { .. } => "request_template",
            },
            "headerCount": headers.len(),
        }),
        HttpRouteAction::StaticFiles {
            root_directory: _,
            path_mapping,
            index_files,
            spa_fallback,
            try_files,
            autoindex,
            autoindex_exact_size,
            autoindex_local_time,
            mime,
            headers,
            error_responses,
            ..
        } => json!({
            "type": "static_files",
            "pathMapping": match path_mapping {
                oxiroute_config::HttpStaticPathMapping::Root => "root",
                oxiroute_config::HttpStaticPathMapping::Alias => "alias",
            },
            "indexFiles": index_files,
            "spaFallback": spa_fallback.is_some(),
            "tryFileCount": try_files.len(),
            "autoindex": autoindex,
            "autoindexExactSize": autoindex_exact_size,
            "autoindexLocalTime": autoindex_local_time,
            "mimeMappingCount": mime.types.len(),
            "defaultType": mime.default_type,
            "headerCount": headers.len(),
            "errorResponseCount": error_responses.len(),
        }),
    }
}

pub(super) fn certificate_source(source: &CertificateSource) -> Value {
    match source {
        CertificateSource::Files {
            certificate_chain_path,
            private_key_path: _,
        } => json!({
            "type": "files",
            "certificateChainPath": certificate_chain_path,
            "privateKeyPath": "<redacted>",
        }),
        CertificateSource::Certbot {
            live_directory_path,
            archive_directory_path,
        } => json!({
            "type": "certbot",
            "liveDirectoryPath": live_directory_path,
            "archiveDirectoryPath": archive_directory_path,
        }),
        CertificateSource::AcmeManaged {
            directory_url,
            state_root,
            contacts,
            terms_agreed,
            challenge,
            key_type,
            allowed_dns_suffixes,
            dns01,
            retained_revisions,
            retention_days,
        } => json!({
            "type": "acme_managed",
            "directoryUrl": directory_url,
            "stateRoot": state_root,
            "contactCount": contacts.len(),
            "termsAgreed": terms_agreed,
            "challenge": match challenge {
                oxiroute_config::AcmeChallengeType::Http01 => "http01",
                oxiroute_config::AcmeChallengeType::Dns01 => "dns01",
                oxiroute_config::AcmeChallengeType::TlsAlpn01 => "tls_alpn01",
            },
            "keyType": match key_type {
                oxiroute_config::AcmeKeyType::EcdsaP256 => "ecdsa_p256",
                oxiroute_config::AcmeKeyType::Rsa2048 => "rsa_2048",
            },
            "allowedDnsSuffixCount": allowed_dns_suffixes.len(),
            "retainedRevisions": retained_revisions,
            "retentionDays": retention_days,
            "dnsProvider": dns01.as_ref().map(|dns01| &dns01.provider),
        }),
        CertificateSource::SelfSignedDevelopment {
            validity_days,
            key_type,
        } => json!({
            "type": "self_signed_development",
            "developmentOnly": true,
            "validityDays": validity_days,
            "keyType": match key_type {
                oxiroute_config::SelfSignedKeyType::EcdsaP256 => "ecdsa_p256",
                oxiroute_config::SelfSignedKeyType::Rsa2048 => "rsa_2048",
            },
        }),
    }
}

pub(super) const fn tls_version(version: TlsVersion) -> &'static str {
    match version {
        TlsVersion::Tls12 => "1.2",
        TlsVersion::Tls13 => "1.3",
    }
}

pub(super) const fn alpn_protocol(protocol: AlpnProtocol) -> &'static str {
    match protocol {
        AlpnProtocol::H3 => "h3",
        AlpnProtocol::H2 => "h2",
        AlpnProtocol::Http11 => "http/1.1",
    }
}

pub(super) const fn tls_client_auth_mode(mode: oxiroute_config::TlsClientAuthMode) -> &'static str {
    match mode {
        oxiroute_config::TlsClientAuthMode::Disabled => "disabled",
        oxiroute_config::TlsClientAuthMode::Optional => "optional",
        oxiroute_config::TlsClientAuthMode::Required => "required",
    }
}

pub(super) const fn upstream_algorithm(algorithm: &UpstreamAlgorithm) -> &'static str {
    match algorithm {
        UpstreamAlgorithm::RoundRobin => "round_robin",
        UpstreamAlgorithm::WeightedRoundRobin { .. } => "weighted_round_robin",
        UpstreamAlgorithm::LeastConnections => "least_connections",
        UpstreamAlgorithm::PowerOfTwoChoices => "power_of_two_choices",
        UpstreamAlgorithm::First => "first",
    }
}

pub(super) fn listener_bind(bind: &ListenerBind) -> Value {
    match bind {
        ListenerBind::Socket { address } => json!({
            "type": "socket",
            "address": address,
        }),
        ListenerBind::Udp { address } => json!({
            "type": "udp",
            "address": address,
        }),
        ListenerBind::Unix { path, mode } => json!({
            "type": "unix",
            "path": path,
            "mode": mode,
        }),
    }
}

pub(super) fn endpoint_attributes(endpoint: &UpstreamEndpoint) -> Value {
    match endpoint {
        UpstreamEndpoint::Socket { address } => json!({
            "type": "socket",
            "address": address,
        }),
        UpstreamEndpoint::Dns { host, port } => json!({
            "type": "dns",
            "host": host,
            "port": port,
        }),
        UpstreamEndpoint::Unix { path } => json!({
            "type": "unix",
            "path": path,
        }),
    }
}

pub(super) const fn protocol(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Http => "http",
        Protocol::Rtmp => "rtmp",
        Protocol::Tcp => "tcp",
        Protocol::Udp => "udp",
        Protocol::ForwardHttp1 => "forward_http1",
        Protocol::ForwardHttp2 => "forward_http2",
        Protocol::ForwardHttp3 => "forward_http3",
        Protocol::Http3 => "http3",
    }
}

pub(super) fn health_check(check: &HealthCheck) -> Value {
    json!({
        "type": match check.kind {
            HealthCheckType::Http => "http",
            HealthCheckType::Tcp => "tcp",
        },
        "intervalMs": check.interval_ms,
        "timeoutMs": check.timeout_ms,
        "healthyThreshold": check.healthy_threshold,
        "unhealthyThreshold": check.unhealthy_threshold,
        "host": check.host,
        "path": check.path,
    })
}

pub(super) fn upstream_tls(tls: &UpstreamTls) -> Value {
    json!({
        "serverName": tls.server_name,
        "caCertificatePath": tls.ca_certificate_path,
    })
}

pub(super) const fn http_version(version: HttpVersion) -> &'static str {
    match version {
        HttpVersion::Http11 => "1.1",
        HttpVersion::Http2 => "2",
        HttpVersion::Http3 => "3",
    }
}

pub(super) const fn endpoint_state(state: crate::EndpointHealthState) -> &'static str {
    match state {
        crate::EndpointHealthState::Unchecked => "unchecked",
        crate::EndpointHealthState::Unknown => "unknown",
        crate::EndpointHealthState::Healthy => "healthy",
        crate::EndpointHealthState::Unhealthy => "unhealthy",
    }
}
