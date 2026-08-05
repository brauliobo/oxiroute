use std::{
    fmt::{Display, Write as _},
    path::Path,
};

use crate::{
    defaults::MAX_SOURCE_BYTES,
    model::{
        AccessLogPolicy, AlpnProtocol, CacheAuthorizationPolicy, CacheKeyComponent, CachePredicate,
        CachePurgeAuthorization, CacheSetCookiePolicy, CacheStaleTrigger, CacheStatusTtl,
        CacheStore, CacheSurrogateTags, CacheVaryPolicy, Certificate, CertificateSource, Config,
        ConfigError, DnsResolutionPolicy, DownstreamTimeoutPolicy, ForwardAccessAction,
        ForwardAccessMatcher, ForwardAccessPolicy, ForwardAccessRule, ForwardAuditMode,
        ForwardConnectPolicy, ForwardDestinationPolicy, ForwardDirectFallback, ForwardHeaderPolicy,
        ForwardHttpVersion, ForwardPeerPolicy, ForwardProxyAuth, ForwardProxyService,
        ForwardResolverPolicy, ForwardTimeRange,
        ForwardViaPolicy, ForwardWeekday, ForwardedForPolicy, HealthCheck, HealthCheckType,
        HealthHttpVersion, HealthStartup, HttpAccessPolicy, HttpCachePolicy,
        HttpCookieAttributePolicy, HttpCookiePathRewrite, HttpGzipPolicy, HttpHostSelector,
        HttpLiteralHeader, HttpMimeType, HttpPathSelector, HttpProxyPathRewrite, HttpProxyPolicy,
        HttpRedirectLocation,
        HttpRequestHeaderMutation, HttpRequestHeaderValue, HttpResponseHeaderMutation,
        HttpRetryBodySafety, HttpRetryMethodSafety, HttpRetryPolicy, HttpRetryTarget,
        HttpRetryTrigger, HttpRoute, HttpRouteAction, HttpRoutePolicy, HttpSameSite, HttpService,
        HttpStaticErrorResponse, HttpStaticMimePolicy, HttpStaticPathMapping, HttpStaticTryFile,
        HttpUpstreamHost, HttpVersion, HttpVersionPolicy, L4Service, Listener, ListenerBind,
         Management, Protocol, RtmpAccessPolicy, RtmpAccessRule, RtmpAclAction, RtmpApplication,
         RtmpCallbackConfig, RtmpFanoutPolicy, RtmpNotifyMethod, RtmpPullTarget, RtmpPushTarget,
         RtmpExecEnvironment, RtmpExecFilesystemPolicy, RtmpExecMode, RtmpExecNetworkPolicy,
         RtmpExecProfile, RtmpExecTrigger,
         RtmpRecorder, RtmpRecorderSegmentNaming, RtmpRecorderStart, RtmpRecorderTimeBasis,
         RtmpRecorderTimezone, RtmpRelayPolicy, RtmpRtmpsPolicy, RtmpService, RtmpSessionCeilings,
         RtmpTokenPolicy, RtmpTokenSource, RtmpTransport, RtmpVodPolicy, RtmpVodSource,
          RtmpHlsFragmentNaming, RtmpHlsKeyPolicy, RtmpHlsPolicy, RtmpHlsVariant, RtmpDashPolicy,
          RtmpDashSegmentNaming,
         Stats,
        StatsPage, StatsPageAdminPolicy, TlsProfile, TlsVersion, UdpPolicy, UpstreamAlgorithm,
        UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool, UpstreamServer, UpstreamTls,
        ProxyProtocolPolicy, ProxyProtocolVersion,
    },
    validation::validate_config,
};

/// Renders a validated, normalized configuration as deterministic data-only Lua.
///
/// Every current field is emitted, including defaults, empty collections, and explicit `nil` or
/// `null` options. Collection order is preserved.
///
/// # Errors
///
/// Returns an error when the configuration is invalid, contains a non-UTF-8 path, or renders beyond
/// the loader's source-size limit.
pub fn render_lua(config: &Config) -> Result<String, ConfigError> {
    let mut config = config.clone();
    validate_config(&mut config)?;

    let mut renderer = Renderer::new();
    renderer.config(&config)?;
    let output = renderer.finish();
    if output.len() > MAX_SOURCE_BYTES {
        return Err(ConfigError::SourceTooLarge);
    }

    Ok(output)
}

struct Renderer {
    output: String,
    indent: usize,
}

impl Renderer {
    fn new() -> Self {
        Self {
            output: String::new(),
            indent: 0,
        }
    }

    fn finish(self) -> String {
        self.output
    }

    fn config(&mut self, config: &Config) -> Result<(), ConfigError> {
        let Config {
            version,
            max_connections,
            management,
            stats,
            certificates,
            tls_profiles,
            listeners,
            cache_stores,
            upstream_pools,
            http_services,
            forward_proxy_services,
            rtmp_services,
            l4_services,
        } = config;

        self.output.push_str("return {\n");
        self.indent = 1;
        self.integer_field("version", version);
        match max_connections {
            Some(limit) => self.integer_field("max_connections", limit),
            None => self.null_field("max_connections"),
        }
        self.fallible_optional_table_field("management", management.as_ref(), Self::management)?;
        self.fallible_optional_table_field("stats", stats.as_ref(), Self::stats)?;
        self.fallible_table_list_field("certificates", certificates, Self::certificate)?;
        self.table_list_field("tls_profiles", tls_profiles, Self::tls_profile);
        self.fallible_table_list_field("listeners", listeners, Self::listener)?;
        self.fallible_table_list_field("cache_stores", cache_stores, Self::cache_store)?;
        self.fallible_table_list_field("upstream_pools", upstream_pools, Self::upstream_pool)?;
        self.fallible_table_list_field("http_services", http_services, Self::http_service)?;
        self.fallible_table_list_field(
            "forward_proxy_services",
            forward_proxy_services,
            Self::forward_proxy_service,
        )?;
        self.fallible_table_list_field("rtmp_services", rtmp_services, Self::rtmp_service)?;
        self.table_list_field("l4_services", l4_services, Self::l4_service);
        self.indent = 0;
        self.output.push_str("}\n");
        Ok(())
    }

    fn management(&mut self, management: &Management) -> Result<(), ConfigError> {
        let Management { bind, ui_dir } = management;

        self.string_field("bind", &bind.to_string());
        match ui_dir {
            Some(path) => {
                self.string_field(
                    "ui_dir",
                    utf8_path(path, "management", "management", "ui_dir")?,
                );
            }
            None => self.nil_field("ui_dir"),
        }
        Ok(())
    }

    fn stats(&mut self, stats: &Stats) -> Result<(), ConfigError> {
        self.string_list_field(
            "binds",
            &stats
                .binds
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        );
        match &stats.admin_token_file {
            Some(path) => self.string_field(
                "admin_token_file",
                utf8_path(path, "stats", "stats", "admin_token_file")?,
            ),
            None => self.nil_field("admin_token_file"),
        }
        self.table_list_field("pages", &stats.pages, Self::stats_page);
        Ok(())
    }

    fn stats_page(&mut self, page: &StatsPage) {
        self.string_field("bind", &page.bind.to_string());
        self.string_field("uri_prefix", &page.uri_prefix);
        self.integer_field("refresh_ms", page.refresh_ms);
        self.string_field(
            "admin",
            match page.admin {
                StatsPageAdminPolicy::Disabled => "disabled",
                StatsPageAdminPolicy::Localhost => "localhost",
            },
        );
        match page.max_connections {
            Some(limit) => self.integer_field("max_connections", limit),
            None => self.null_field("max_connections"),
        }
        self.begin_table_field("downstream_timeouts");
        self.downstream_timeouts(&page.downstream_timeouts);
        self.end_table();
    }

    #[allow(clippy::too_many_lines)]
    fn certificate(&mut self, certificate: &Certificate) -> Result<(), ConfigError> {
        let Certificate {
            name,
            dns_names,
            source,
        } = certificate;

        self.string_field("name", name);
        self.string_list_field("dns_names", dns_names);
        self.begin_table_field("source");
        match source {
            CertificateSource::Files {
                certificate_chain_path,
                private_key_path,
            } => {
                self.string_field("type", "files");
                self.string_field(
                    "certificate_chain_path",
                    utf8_path(
                        certificate_chain_path,
                        "certificate",
                        name,
                        "source.certificate_chain_path",
                    )?,
                );
                self.string_field(
                    "private_key_path",
                    utf8_path(
                        private_key_path,
                        "certificate",
                        name,
                        "source.private_key_path",
                    )?,
                );
            }
            CertificateSource::Certbot {
                live_directory_path,
                archive_directory_path,
            } => {
                self.string_field("type", "certbot");
                self.string_field(
                    "live_directory_path",
                    utf8_path(
                        live_directory_path,
                        "certificate",
                        name,
                        "source.live_directory_path",
                    )?,
                );
                self.string_field(
                    "archive_directory_path",
                    utf8_path(
                        archive_directory_path,
                        "certificate",
                        name,
                        "source.archive_directory_path",
                    )?,
                );
            }
            CertificateSource::AcmeManaged {
                directory_url,
                state_root,
                contacts,
                terms_agreed,
                challenge,
                key_type,
                allowed_dns_suffixes,
                retained_revisions,
                retention_days,
                dns01,
            } => {
                self.string_field("type", "acme_managed");
                self.string_field("directory_url", directory_url);
                self.string_field(
                    "state_root",
                    utf8_path(state_root, "certificate", name, "source.state_root")?,
                );
                self.string_list_field("contacts", contacts);
                self.boolean_field("terms_agreed", *terms_agreed);
                self.string_field(
                    "challenge",
                    match challenge {
                        crate::model::AcmeChallengeType::Http01 => "http01",
                        crate::model::AcmeChallengeType::Dns01 => "dns01",
                        crate::model::AcmeChallengeType::TlsAlpn01 => "tls_alpn01",
                    },
                );
                self.string_field(
                    "key_type",
                    match key_type {
                        crate::model::AcmeKeyType::EcdsaP256 => "ecdsa_p256",
                        crate::model::AcmeKeyType::Rsa2048 => "rsa_2048",
                    },
                );
                self.string_list_field("allowed_dns_suffixes", allowed_dns_suffixes);
                self.integer_field("retained_revisions", u64::from(*retained_revisions));
                self.integer_field("retention_days", u64::from(*retention_days));
                match dns01 {
                    Some(dns01) => {
                        self.begin_table_field("dns01");
                        self.string_field("provider", &dns01.provider);
                        self.string_field(
                            "credential_file",
                            utf8_path(
                                &dns01.credential_file,
                                "certificate",
                                name,
                                "source.dns01.credential_file",
                            )?,
                        );
                        self.integer_field("timeout_seconds", dns01.timeout_seconds);
                        self.end_table();
                    }
                    None => self.nil_field("dns01"),
                }
            }
            CertificateSource::SelfSignedDevelopment {
                validity_days,
                key_type,
            } => {
                self.string_field("type", "self_signed_development");
                self.integer_field("validity_days", validity_days);
                self.string_field(
                    "key_type",
                    match key_type {
                        crate::model::SelfSignedKeyType::EcdsaP256 => "ecdsa_p256",
                        crate::model::SelfSignedKeyType::Rsa2048 => "rsa_2048",
                    },
                );
            }
        }
        self.end_table();
        Ok(())
    }

    fn tls_profile(&mut self, profile: &TlsProfile) {
        let TlsProfile {
            name,
            certificates,
            default_certificate,
            min_version,
            alpn,
            policy,
        } = profile;

        self.string_field("name", name);
        self.string_list_field("certificates", certificates);
        self.string_field("default_certificate", default_certificate);
        self.string_field(
            "min_version",
            match min_version {
                TlsVersion::Tls12 => "1.2",
                TlsVersion::Tls13 => "1.3",
            },
        );
        self.string_list_field(
            "alpn",
            &alpn
                .iter()
                .map(|protocol| match protocol {
                    AlpnProtocol::H3 => "h3",
                    AlpnProtocol::H2 => "h2",
                    AlpnProtocol::Http11 => "http/1.1",
                })
                .collect::<Vec<_>>(),
        );
        self.begin_table_field("policy");
        match &policy.cipher_list {
            Some(cipher_list) => self.string_field("cipher_list", cipher_list),
            None => self.nil_field("cipher_list"),
        }
        match &policy.dh_parameters_path {
            Some(path) => self.string_field(
                "dh_parameters_path",
                utf8_path(path, "TLS profile", name, "policy.dh_parameters_path")
                    .expect("validated TLS DH parameters path"),
            ),
            None => self.nil_field("dh_parameters_path"),
        }
        match &policy.session_cache {
            Some(cache) => {
                self.begin_table_field("session_cache");
                self.string_field("name", &cache.name);
                self.integer_field("size_bytes", cache.size_bytes);
                self.end_table();
            }
            None => self.nil_field("session_cache"),
        }
        match policy.session_timeout_seconds {
            Some(seconds) => self.integer_field("session_timeout_seconds", seconds),
            None => self.nil_field("session_timeout_seconds"),
        }
        self.boolean_field("session_tickets", policy.session_tickets);
        self.boolean_field("prefer_server_ciphers", policy.prefer_server_ciphers);
        self.begin_table_field("client_auth");
        self.string_field(
            "mode",
            match policy.client_auth.mode {
                crate::model::TlsClientAuthMode::Disabled => "disabled",
                crate::model::TlsClientAuthMode::Optional => "optional",
                crate::model::TlsClientAuthMode::Required => "required",
            },
        );
        match &policy.client_auth.ca_certificate_path {
            Some(path) => self.string_field(
                "ca_certificate_path",
                utf8_path(
                    path,
                    "TLS profile",
                    name,
                    "policy.client_auth.ca_certificate_path",
                )
                .expect("validated TLS client CA path"),
            ),
            None => self.nil_field("ca_certificate_path"),
        }
        self.string_list_field("allowed_dns_names", &policy.client_auth.allowed_dns_names);
        self.end_table();
        self.end_table();
    }

    fn listener(&mut self, listener: &Listener) -> Result<(), ConfigError> {
        let Listener {
            name,
            bind,
            protocol,
            service,
            tls_profile,
            proxy_protocol,
            max_connections,
            downstream_timeouts,
        } = listener;

        self.string_field("name", name);
        self.begin_table_field("bind");
        match bind {
            ListenerBind::Socket { address } => {
                self.string_field("type", "socket");
                self.string_field("address", &address.to_string());
            }
            ListenerBind::Udp { address } => {
                self.string_field("type", "udp");
                self.string_field("address", &address.to_string());
            }
            ListenerBind::Unix { path, mode } => {
                self.string_field("type", "unix");
                self.string_field("path", utf8_path(path, "listener", name, "bind.path")?);
                match mode {
                    Some(mode) => self.integer_field("mode", mode),
                    None => self.nil_field("mode"),
                }
            }
        }
        self.end_table();
        self.string_field(
            "protocol",
            match protocol {
                Protocol::Http => "http",
                Protocol::Rtmp => "rtmp",
                Protocol::Tcp => "tcp",
                Protocol::Udp => "udp",
                Protocol::ForwardHttp1 => "forward_http1",
                Protocol::ForwardHttp2 => "forward_http2",
                Protocol::ForwardHttp3 => "forward_http3",
                Protocol::Http3 => "http3",
            },
        );
        self.optional_string_field("service", service.as_deref());
        self.optional_string_field("tls_profile", tls_profile.as_deref());
        self.optional_table_field(
            "proxy_protocol",
            proxy_protocol.as_ref(),
            Self::proxy_protocol,
        );
        match max_connections {
            Some(max_connections) => self.integer_field("max_connections", max_connections),
            None => self.null_field("max_connections"),
        }
        self.begin_table_field("downstream_timeouts");
        self.downstream_timeouts(downstream_timeouts);
        self.end_table();
        Ok(())
    }

    fn downstream_timeouts(&mut self, policy: &DownstreamTimeoutPolicy) {
        self.optional_integer_field("client_timeout_ms", policy.client_timeout_ms);
        self.optional_integer_field("request_timeout_ms", policy.request_timeout_ms);
        self.optional_integer_field("keepalive_timeout_ms", policy.keepalive_timeout_ms);
    }

    fn proxy_protocol(&mut self, policy: &ProxyProtocolPolicy) {
        self.string_field(
            "version",
            match policy.version {
                ProxyProtocolVersion::V1 => "v1",
                ProxyProtocolVersion::V2 => "v2",
                ProxyProtocolVersion::Auto => "auto",
            },
        );
        self.integer_field("timeout_ms", policy.timeout_ms);
    }

    fn cache_store(&mut self, store: &CacheStore) -> Result<(), ConfigError> {
        match store {
            CacheStore::Memory {
                name,
                max_bytes,
                max_entries,
                max_object_bytes,
                max_header_bytes,
                max_key_bytes,
                max_tag_bytes,
                max_tags_per_object,
                max_in_flight_fills,
                max_followers_per_fill,
            } => {
                self.string_field("type", "memory");
                self.string_field("name", name);
                self.integer_field("max_bytes", max_bytes);
                self.integer_field("max_entries", max_entries);
                self.cache_store_common(
                    *max_object_bytes,
                    *max_header_bytes,
                    *max_key_bytes,
                    *max_tag_bytes,
                    *max_tags_per_object,
                    *max_in_flight_fills,
                    *max_followers_per_fill,
                );
            }
            CacheStore::Disk {
                name,
                root_directory,
                max_bytes,
                max_files,
                max_object_bytes,
                max_header_bytes,
                max_key_bytes,
                max_tag_bytes,
                max_tags_per_object,
                max_in_flight_fills,
                max_followers_per_fill,
            } => {
                self.string_field("type", "disk");
                self.string_field("name", name);
                let root =
                    root_directory
                        .to_str()
                        .ok_or_else(|| ConfigError::InvalidCacheStore {
                            store: name.clone(),
                            field: "root_directory",
                            detail: "path must be valid UTF-8".into(),
                        })?;
                self.string_field("root_directory", root);
                self.integer_field("max_bytes", max_bytes);
                self.integer_field("max_files", max_files);
                self.cache_store_common(
                    *max_object_bytes,
                    *max_header_bytes,
                    *max_key_bytes,
                    *max_tag_bytes,
                    *max_tags_per_object,
                    *max_in_flight_fills,
                    *max_followers_per_fill,
                );
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn cache_store_common(
        &mut self,
        max_object_bytes: u64,
        max_header_bytes: u64,
        max_key_bytes: u64,
        max_tag_bytes: u64,
        max_tags_per_object: u64,
        max_in_flight_fills: u64,
        max_followers_per_fill: u64,
    ) {
        self.integer_field("max_object_bytes", max_object_bytes);
        self.integer_field("max_header_bytes", max_header_bytes);
        self.integer_field("max_key_bytes", max_key_bytes);
        self.integer_field("max_tag_bytes", max_tag_bytes);
        self.integer_field("max_tags_per_object", max_tags_per_object);
        self.integer_field("max_in_flight_fills", max_in_flight_fills);
        self.integer_field("max_followers_per_fill", max_followers_per_fill);
    }

    fn rtmp_service(&mut self, service: &RtmpService) -> Result<(), ConfigError> {
        let RtmpService {
            name,
            outbound_chunk_size,
            access_log,
            outbound_policy,
            callbacks,
            exec_profiles,
            applications,
        } = service;
        self.string_field("name", name);
        self.integer_field("outbound_chunk_size", outbound_chunk_size);
        self.access_log_field("access_log", access_log.as_ref(), "RTMP service", name)?;
        self.begin_table_field("outbound_policy");
        self.rtmp_outbound_policy(outbound_policy);
        self.end_table();
        self.begin_table_field("callbacks");
        self.rtmp_callbacks(callbacks);
        self.end_table();
        self.fallible_table_list_field("exec_profiles", exec_profiles, Self::rtmp_exec_profile)?;
        self.fallible_table_list_field("applications", applications, |renderer, application| {
            renderer.rtmp_application(name, application)
        })?;
        Ok(())
    }

    fn rtmp_exec_profile(&mut self, profile: &RtmpExecProfile) -> Result<(), ConfigError> {
        self.string_field("name", &profile.name);
        self.string_field("application", &profile.application);
        self.string_field(
            "mode",
            match profile.mode {
                RtmpExecMode::Command => "command",
                RtmpExecMode::Transcode => "transcode",
            },
        );
        self.string_field(
            "trigger",
            match profile.trigger {
                RtmpExecTrigger::Publisher => "publisher",
                RtmpExecTrigger::PublishDone => "publish_done",
            },
        );
        self.string_field(
            "executable",
            utf8_path(
                &profile.executable,
                "RTMP exec profile",
                &profile.name,
                "exec_profiles[].executable",
            )?,
        );
        self.string_list_field("arguments", &profile.arguments);
        self.table_list_field("environment", &profile.environment, Self::rtmp_exec_environment);
        self.string_field(
            "working_directory",
            utf8_path(
                &profile.working_directory,
                "RTMP exec profile",
                &profile.name,
                "exec_profiles[].working_directory",
            )?,
        );
        self.string_field(
            "filesystem",
            match profile.filesystem {
                RtmpExecFilesystemPolicy::WorkingDirectory => "working_directory",
                RtmpExecFilesystemPolicy::Host => "host",
            },
        );
        self.string_field(
            "network",
            match profile.network {
                RtmpExecNetworkPolicy::Disabled => "disabled",
                RtmpExecNetworkPolicy::Inherited => "inherited",
            },
        );
        self.integer_field("timeout_ms", profile.timeout_ms);
        self.integer_field("shutdown_timeout_ms", profile.shutdown_timeout_ms);
        self.integer_field("max_processes", profile.max_processes);
        self.integer_field("max_queue_messages", profile.max_queue_messages);
        self.integer_field("max_queue_bytes", profile.max_queue_bytes);
        self.integer_field("max_stdout_bytes", profile.max_stdout_bytes);
        self.integer_field("max_stderr_bytes", profile.max_stderr_bytes);
        self.boolean_field("respawn", profile.respawn);
        self.integer_field("respawn_delay_ms", profile.respawn_delay_ms);
        self.integer_field("max_respawns", profile.max_respawns);
        Ok(())
    }

    fn rtmp_exec_environment(&mut self, environment: &RtmpExecEnvironment) {
        self.string_field("name", &environment.name);
        self.string_field("value", &environment.value);
    }

    fn rtmp_application(
        &mut self,
        service_name: &str,
        application: &RtmpApplication,
    ) -> Result<(), ConfigError> {
        let RtmpApplication {
            name,
            live,
            idle_streams,
            publish,
            play,
            limits,
            push_targets,
            pull_targets,
            relay,
            callbacks,
            fanout,
            vod,
            hls,
            dash,
            recorders,
        } = application;
        self.string_field("name", name);
        self.boolean_field("live", *live);
        self.boolean_field("idle_streams", *idle_streams);
        self.begin_table_field("publish");
        self.rtmp_access_policy(publish);
        self.end_table();
        self.begin_table_field("play");
        self.rtmp_access_policy(play);
        self.end_table();
        self.begin_table_field("limits");
        self.rtmp_session_ceilings(limits);
        self.end_table();
        self.table_list_or_nil_field("push_targets", push_targets, Self::rtmp_push_target);
        self.table_list_or_nil_field("pull_targets", pull_targets, Self::rtmp_pull_target);
        self.begin_table_field("relay");
        self.rtmp_relay_policy(relay);
        self.end_table();
        self.begin_table_field("callbacks");
        self.rtmp_callbacks(callbacks);
        self.end_table();
        self.begin_table_field("fanout");
        self.rtmp_fanout(fanout);
        self.end_table();
        self.optional_table_field("vod", vod.as_ref(), Self::rtmp_vod_policy);
        self.fallible_optional_table_field("hls", hls.as_ref(), Self::rtmp_hls_policy)?;
        self.fallible_optional_table_field("dash", dash.as_ref(), Self::rtmp_dash_policy)?;
        self.fallible_table_list_field("recorders", recorders, |renderer, recorder| {
            renderer.rtmp_recorder(service_name, name, recorder)
        })?;
        Ok(())
    }

    fn rtmp_vod_policy(&mut self, policy: &RtmpVodPolicy) {
        self.integer_field("max_sessions", policy.max_sessions);
        self.integer_field("max_file_bytes", policy.max_file_bytes);
        self.integer_field("max_duration_ms", policy.max_duration_ms);
        self.table_list_field("sources", &policy.sources, Self::rtmp_vod_source);
    }

    fn rtmp_vod_source(&mut self, source: &RtmpVodSource) {
        match source {
            RtmpVodSource::Local {
                name,
                root_directory,
            } => {
                self.string_field("type", "local");
                self.string_field("name", name);
                self.string_field(
                    "root_directory",
                    root_directory
                        .to_str()
                        .expect("validated VOD root directory is UTF-8"),
                );
            }
            RtmpVodSource::Http { name, origin } => {
                self.string_field("type", "http");
                self.string_field("name", name);
                self.string_field("origin", origin);
            }
        }
    }

    fn rtmp_hls_policy(&mut self, policy: &RtmpHlsPolicy) -> Result<(), ConfigError> {
        self.string_field(
            "root_directory",
            utf8_path(&policy.root_directory, "RTMP HLS", "hls", "root_directory")?,
        );
        self.integer_field("segment_duration_ms", policy.segment_duration_ms);
        self.integer_field("max_segment_duration_ms", policy.max_segment_duration_ms);
        self.integer_field("playlist_length_ms", policy.playlist_length_ms);
        self.string_field(
            "fragment_naming",
            match policy.fragment_naming {
                RtmpHlsFragmentNaming::Sequential => "sequential",
                RtmpHlsFragmentNaming::Timestamp => "timestamp",
                RtmpHlsFragmentNaming::System => "system",
            },
        );
        self.boolean_field("nested", policy.nested);
        self.boolean_field("cleanup", policy.cleanup);
        self.fallible_table_list_field("variants", &policy.variants, Self::rtmp_hls_variant)?;
        self.fallible_optional_table_field("keys", policy.keys.as_ref(), Self::rtmp_hls_keys)?;
        self.integer_field("max_segment_bytes", policy.max_segment_bytes);
        self.integer_field("max_queue_messages", policy.max_queue_messages);
        self.integer_field("max_storage_bytes", policy.max_storage_bytes);
        self.integer_field("max_storage_files", policy.max_storage_files);
        self.integer_field("max_active_streams", policy.max_active_streams);
        Ok(())
    }

    #[allow(clippy::unnecessary_wraps)]
    fn rtmp_hls_variant(&mut self, variant: &RtmpHlsVariant) -> Result<(), ConfigError> {
        self.string_field("name", &variant.name);
        self.integer_field("bandwidth", variant.bandwidth);
        self.optional_string_field("codecs", variant.codecs.as_deref());
        match variant.width {
            Some(width) => self.integer_field("width", width),
            None => self.null_field("width"),
        }
        match variant.height {
            Some(height) => self.integer_field("height", height),
            None => self.null_field("height"),
        }
        Ok(())
    }

    #[allow(clippy::unnecessary_wraps)]
    fn rtmp_hls_keys(&mut self, keys: &RtmpHlsKeyPolicy) -> Result<(), ConfigError> {
        self.integer_field("rotation_segments", keys.rotation_segments);
        self.string_field("url_prefix", &keys.url_prefix);
        Ok(())
    }

    fn rtmp_dash_policy(&mut self, policy: &RtmpDashPolicy) -> Result<(), ConfigError> {
        self.string_field(
            "root_directory",
            utf8_path(&policy.root_directory, "RTMP DASH", "dash", "root_directory")?,
        );
        self.integer_field("segment_duration_ms", policy.segment_duration_ms);
        self.integer_field("max_segment_duration_ms", policy.max_segment_duration_ms);
        self.integer_field("playlist_length_ms", policy.playlist_length_ms);
        self.string_field(
            "segment_naming",
            match policy.segment_naming {
                RtmpDashSegmentNaming::Sequential => "sequential",
                RtmpDashSegmentNaming::Timestamp => "timestamp",
                RtmpDashSegmentNaming::System => "system",
            },
        );
        self.boolean_field("nested", policy.nested);
        self.boolean_field("cleanup", policy.cleanup);
        self.integer_field("max_segment_bytes", policy.max_segment_bytes);
        self.integer_field("max_queue_messages", policy.max_queue_messages);
        self.integer_field("max_storage_bytes", policy.max_storage_bytes);
        self.integer_field("max_storage_files", policy.max_storage_files);
        self.integer_field("max_active_streams", policy.max_active_streams);
        Ok(())
    }

    fn rtmp_push_target(&mut self, target: &RtmpPushTarget) {
        self.string_field("host", &target.host);
        self.integer_field("port", target.port);
        self.string_field("application", &target.application);
        self.string_field(
            "scheme",
            match target.scheme {
                RtmpTransport::Rtmp => "rtmp",
                RtmpTransport::Rtmps => "rtmps",
            },
        );
        self.optional_string_field("stream_name", target.stream_name.as_deref());
        self.optional_string_field("tc_url", target.tc_url.as_deref());
        self.optional_string_field("flash_version", target.flash_version.as_deref());
        self.optional_table_field(
            "credentials",
            target.credentials.as_ref(),
            Self::rtmp_credentials,
        );
    }

    fn rtmp_pull_target(&mut self, target: &RtmpPullTarget) {
        self.string_field("host", &target.host);
        self.integer_field("port", target.port);
        self.string_field("application", &target.application);
        self.string_field("stream_name", &target.stream_name);
        self.string_field(
            "scheme",
            match target.scheme {
                RtmpTransport::Rtmp => "rtmp",
                RtmpTransport::Rtmps => "rtmps",
            },
        );
        self.optional_string_field("tc_url", target.tc_url.as_deref());
        self.optional_string_field("flash_version", target.flash_version.as_deref());
        self.optional_table_field(
            "credentials",
            target.credentials.as_ref(),
            Self::rtmp_credentials,
        );
    }

    fn rtmp_credentials(&mut self, credentials: &crate::model::RtmpCredentialReference) {
        self.string_field("username", &credentials.username);
        self.string_field(
            "secret_file",
            credentials
                .secret_file
                .to_str()
                .expect("validated RTMP credential path is UTF-8"),
        );
    }

    fn rtmp_outbound_policy(&mut self, policy: &crate::model::RtmpOutboundPolicy) {
        self.string_list_field("allow_domains", &policy.allow_domains);
        self.string_list_field("deny_domains", &policy.deny_domains);
        self.string_list_field("allow_cidrs", &policy.allow_cidrs);
        self.string_list_field("deny_cidrs", &policy.deny_cidrs);
        self.boolean_field("deny_private", policy.deny_private);
        self.string_field(
            "rtmps",
            match policy.rtmps {
                RtmpRtmpsPolicy::Disabled => "disabled",
                RtmpRtmpsPolicy::Allowed => "allowed",
                RtmpRtmpsPolicy::Required => "required",
            },
        );
        self.integer_field("max_chain_depth", policy.max_chain_depth);
    }

    fn rtmp_relay_policy(&mut self, policy: &RtmpRelayPolicy) {
        self.integer_field("max_queue_messages", policy.max_queue_messages);
        self.integer_field("max_queue_bytes", policy.max_queue_bytes);
        self.integer_field("buffer_ms", policy.buffer_ms);
        self.integer_field("push_reconnect_ms", policy.push_reconnect_ms);
        self.integer_field("pull_reconnect_ms", policy.pull_reconnect_ms);
        self.integer_field("connect_timeout_ms", policy.connect_timeout_ms);
        self.integer_field("handshake_timeout_ms", policy.handshake_timeout_ms);
    }

    fn rtmp_callbacks(&mut self, callbacks: &RtmpCallbackConfig) {
        self.optional_string_field("on_connect", callbacks.on_connect.as_deref());
        self.optional_string_field("on_disconnect", callbacks.on_disconnect.as_deref());
        self.optional_string_field("on_publish", callbacks.on_publish.as_deref());
        self.optional_string_field("on_publish_done", callbacks.on_publish_done.as_deref());
        self.optional_string_field("on_play", callbacks.on_play.as_deref());
        self.optional_string_field("on_play_done", callbacks.on_play_done.as_deref());
        self.optional_string_field("on_done", callbacks.on_done.as_deref());
        self.optional_string_field("on_update", callbacks.on_update.as_deref());
        self.string_field(
            "notify_method",
            match callbacks.notify_method {
                RtmpNotifyMethod::Get => "get",
                RtmpNotifyMethod::Post => "post",
            },
        );
        self.integer_field("timeout_ms", callbacks.timeout_ms);
        self.integer_field(
            "notify_update_timeout_ms",
            callbacks.notify_update_timeout_ms,
        );
        self.boolean_field("notify_update_strict", callbacks.notify_update_strict);
        self.boolean_field("notify_relay_redirect", callbacks.notify_relay_redirect);
    }

    fn rtmp_fanout(&mut self, policy: &RtmpFanoutPolicy) {
        self.integer_field("max_subscribers", policy.max_subscribers);
        self.integer_field(
            "max_queue_messages_per_subscriber",
            policy.max_queue_messages_per_subscriber,
        );
        self.integer_field(
            "max_queue_bytes_per_subscriber",
            policy.max_queue_bytes_per_subscriber,
        );
    }

    fn rtmp_access_policy(&mut self, policy: &RtmpAccessPolicy) {
        self.table_list_field("rules", &policy.rules, Self::rtmp_access_rule);
        self.optional_table_field("token", policy.token.as_ref(), Self::rtmp_token_policy);
    }

    fn rtmp_access_rule(&mut self, rule: &RtmpAccessRule) {
        self.string_field(
            "action",
            match rule.action {
                RtmpAclAction::Allow => "allow",
                RtmpAclAction::Deny => "deny",
            },
        );
        self.string_field("network", &rule.network);
    }

    fn rtmp_token_policy(&mut self, token: &RtmpTokenPolicy) {
        self.string_field(
            "source",
            match token.source {
                RtmpTokenSource::StreamQuery => "stream_query",
            },
        );
        self.string_field("parameter", &token.parameter);
        self.string_field("secret", &token.secret);
    }

    fn rtmp_session_ceilings(&mut self, limits: &RtmpSessionCeilings) {
        self.integer_field("max_connections", limits.max_connections);
        self.integer_field("max_publishers", limits.max_publishers);
        self.integer_field("max_viewers", limits.max_viewers);
    }

    fn rtmp_recorder(
        &mut self,
        service_name: &str,
        application_name: &str,
        recorder: &RtmpRecorder,
    ) -> Result<(), ConfigError> {
        let RtmpRecorder {
            name,
            start,
            root_directory,
            record_mask,
            suffix_template,
            append_unix_seconds,
            append,
            lock,
            max_size,
            max_frames,
            notify,
            timezone,
            time_basis,
            segment_naming,
            rotation_interval_ms,
            max_queue_messages,
            max_queue_bytes,
            shutdown_timeout_ms,
            max_storage_bytes,
            max_storage_files,
            max_active_recorders,
        } = recorder;
        self.string_field("name", name);
        self.string_field(
            "start",
            match start {
                RtmpRecorderStart::Continuous => "continuous",
                RtmpRecorderStart::Manual => "manual",
            },
        );
        self.string_field(
            "root_directory",
            utf8_recording_root(root_directory, service_name, application_name, name)?,
        );
        self.begin_table_field("record_mask");
        self.boolean_field("audio", record_mask.audio);
        self.boolean_field("video", record_mask.video);
        self.boolean_field("keyframes", record_mask.keyframes);
        self.end_table();
        self.string_field("suffix_template", suffix_template);
        self.boolean_field("append_unix_seconds", *append_unix_seconds);
        self.boolean_field("append", *append);
        self.boolean_field("lock", *lock);
        match max_size {
            Some(limit) => self.integer_field("max_size", limit),
            None => self.null_field("max_size"),
        }
        match max_frames {
            Some(limit) => self.integer_field("max_frames", limit),
            None => self.null_field("max_frames"),
        }
        self.boolean_field("notify", *notify);
        self.string_field(
            "timezone",
            match timezone {
                RtmpRecorderTimezone::Utc => "utc",
                RtmpRecorderTimezone::Iana(name) => name,
            },
        );
        self.string_field(
            "time_basis",
            match time_basis {
                RtmpRecorderTimeBasis::SegmentStart => "segment_start",
                RtmpRecorderTimeBasis::SegmentEnd => "segment_end",
            },
        );
        self.string_field(
            "segment_naming",
            match segment_naming {
                RtmpRecorderSegmentNaming::SafeUnique => "safe_unique",
                RtmpRecorderSegmentNaming::NginxCompatible => "nginx_compatible",
            },
        );
        match rotation_interval_ms {
            Some(interval) => self.integer_field("rotation_interval_ms", interval),
            None => self.null_field("rotation_interval_ms"),
        }
        self.integer_field("max_queue_messages", max_queue_messages);
        self.integer_field("max_queue_bytes", max_queue_bytes);
        self.integer_field("shutdown_timeout_ms", shutdown_timeout_ms);
        match max_storage_bytes {
            Some(limit) => self.integer_field("max_storage_bytes", limit),
            None => self.null_field("max_storage_bytes"),
        }
        match max_storage_files {
            Some(limit) => self.integer_field("max_storage_files", limit),
            None => self.null_field("max_storage_files"),
        }
        self.integer_field("max_active_recorders", max_active_recorders);
        Ok(())
    }

    fn upstream_pool(&mut self, pool: &UpstreamPool) -> Result<(), ConfigError> {
        let UpstreamPool {
            name,
            servers,
            endpoints,
            algorithm,
            health_check,
            tls,
            http_versions,
            queue_timeout_ms,
            connect_timeout_ms,
            server_timeout_ms,
            connection_reuse,
        } = pool;

        self.string_field("name", name);
        debug_assert!(endpoints.is_empty(), "legacy endpoints must be normalized");
        self.fallible_table_list_field("servers", servers, |renderer, server| {
            renderer.upstream_server(name, server)
        })?;
        match algorithm {
            UpstreamAlgorithm::WeightedRoundRobin { weights } => {
                self.begin_table_field("algorithm");
                self.string_field("type", "weighted_round_robin");
                self.integer_list_field("weights", weights);
                self.end_table();
            }
            UpstreamAlgorithm::RoundRobin => self.string_field("algorithm", "round_robin"),
            UpstreamAlgorithm::LeastConnections => {
                self.string_field("algorithm", "least_connections");
            }
            UpstreamAlgorithm::First => self.string_field("algorithm", "first"),
        }
        self.optional_table_field("health_check", health_check.as_ref(), Self::health_check);
        match tls {
            Some(tls) => {
                self.begin_table_field("tls");
                self.upstream_tls(name, tls)?;
                self.end_table();
            }
            None => self.nil_field("tls"),
        }
        self.begin_table_field("http_versions");
        self.http_version_policy(*http_versions);
        self.end_table();
        self.optional_integer_field("queue_timeout_ms", *queue_timeout_ms);
        self.optional_integer_field("connect_timeout_ms", *connect_timeout_ms);
        self.optional_integer_field("server_timeout_ms", *server_timeout_ms);
        self.string_field(
            "connection_reuse",
            match connection_reuse {
                UpstreamConnectionReuse::Never => "never",
                UpstreamConnectionReuse::Safe => "safe",
                UpstreamConnectionReuse::Always => "always",
            },
        );
        Ok(())
    }

    fn upstream_server(
        &mut self,
        pool_name: &str,
        server: &UpstreamServer,
    ) -> Result<(), ConfigError> {
        self.string_field("name", &server.name);
        self.begin_table_field("endpoint");
        self.upstream_endpoint(pool_name, &server.endpoint)?;
        self.end_table();
        match server.max_connections {
            Some(limit) => self.integer_field("max_connections", limit),
            None => self.null_field("max_connections"),
        }
        self.string_field(
            "dns_resolution",
            match server.dns_resolution {
                DnsResolutionPolicy::Startup => "startup",
                DnsResolutionPolicy::OnConnect => "on_connect",
            },
        );
        Ok(())
    }

    fn upstream_endpoint(
        &mut self,
        pool_name: &str,
        endpoint: &UpstreamEndpoint,
    ) -> Result<(), ConfigError> {
        match endpoint {
            UpstreamEndpoint::Socket { address } => {
                self.string_field("type", "socket");
                self.string_field("address", &address.to_string());
            }
            UpstreamEndpoint::Dns { host, port } => {
                self.string_field("type", "dns");
                self.string_field("host", host);
                self.integer_field("port", port);
            }
            UpstreamEndpoint::Unix { path } => {
                self.string_field("type", "unix");
                self.string_field(
                    "path",
                    utf8_path(path, "upstream pool", pool_name, "endpoints[].path")?,
                );
            }
        }
        Ok(())
    }

    fn health_check(&mut self, health_check: &HealthCheck) {
        let HealthCheck {
            kind,
            interval_ms,
            timeout_ms,
            healthy_threshold,
            unhealthy_threshold,
            startup,
            fast_interval_ms,
            down_interval_ms,
            host,
            path,
            expected_status,
            http_version,
        } = health_check;

        self.string_field(
            "type",
            match kind {
                HealthCheckType::Http => "http",
                HealthCheckType::Tcp => "tcp",
            },
        );
        self.integer_field("interval_ms", interval_ms);
        self.integer_field("timeout_ms", timeout_ms);
        self.integer_field("healthy_threshold", healthy_threshold);
        self.integer_field("unhealthy_threshold", unhealthy_threshold);
        self.string_field(
            "startup",
            match startup {
                HealthStartup::Healthy => "healthy",
                HealthStartup::Unhealthy => "unhealthy",
                HealthStartup::Checking => "checking",
            },
        );
        self.optional_integer_field("fast_interval_ms", *fast_interval_ms);
        self.optional_integer_field("down_interval_ms", *down_interval_ms);
        self.optional_string_field("host", host.as_deref());
        self.optional_string_field("path", path.as_deref());
        self.optional_integer_field("expected_status", *expected_status);
        match http_version {
            Some(HealthHttpVersion::Http10) => self.string_field("http_version", "1.0"),
            Some(HealthHttpVersion::Http11) => self.string_field("http_version", "1.1"),
            None => self.nil_field("http_version"),
        }
    }

    fn upstream_tls(&mut self, pool_name: &str, tls: &UpstreamTls) -> Result<(), ConfigError> {
        let UpstreamTls {
            server_name,
            ca_certificate_path,
        } = tls;

        self.string_field("server_name", server_name);
        match ca_certificate_path {
            Some(path) => self.string_field(
                "ca_certificate_path",
                utf8_path(path, "upstream pool", pool_name, "tls.ca_certificate_path")?,
            ),
            None => self.nil_field("ca_certificate_path"),
        }
        Ok(())
    }

    fn http_version_policy(&mut self, policy: HttpVersionPolicy) {
        let HttpVersionPolicy { min, max } = policy;

        self.string_field("min", http_version(min));
        self.string_field("max", http_version(max));
    }

    fn http_service(&mut self, service: &HttpService) -> Result<(), ConfigError> {
        let HttpService {
            name,
            routes,
            automatic_response_headers,
            upstream_io_timeout_ms,
            max_request_body_bytes,
            gzip,
            access_log,
        } = service;

        self.string_field("name", name);
        self.begin_table_field("routes");
        for (route_index, route) in routes.iter().enumerate() {
            self.begin_table_item();
            self.http_route(name, route_index, route)?;
            self.end_table();
        }
        self.end_table();
        self.boolean_field("automatic_response_headers", *automatic_response_headers);
        self.integer_field("upstream_io_timeout_ms", upstream_io_timeout_ms);
        match max_request_body_bytes {
            Some(max_request_body_bytes) => {
                self.integer_field("max_request_body_bytes", max_request_body_bytes);
            }
            None => self.null_field("max_request_body_bytes"),
        }
        match gzip {
            Some(gzip) => {
                self.begin_table_field("gzip");
                self.http_gzip(gzip);
                self.end_table();
            }
            None => self.nil_field("gzip"),
        }
        self.access_log_field("access_log", access_log.as_ref(), "HTTP service", name)?;
        Ok(())
    }

    fn http_gzip(&mut self, gzip: &HttpGzipPolicy) {
        self.integer_field("level", gzip.level);
        self.string_list_field("content_types", &gzip.content_types);
        self.integer_field("min_length_bytes", gzip.min_length_bytes);
        self.string_field(
            "min_http_version",
            match gzip.min_http_version {
                crate::HttpGzipMinimumVersion::Http10 => "1.0",
                crate::HttpGzipMinimumVersion::Http11 => "1.1",
            },
        );
        self.boolean_field("disable_on_via", gzip.disable_on_via);
        self.boolean_field("vary", gzip.vary);
    }

    fn http_route(
        &mut self,
        service: &str,
        route_index: usize,
        route: &HttpRoute,
    ) -> Result<(), ConfigError> {
        let HttpRoute {
            host,
            path,
            methods,
            access_policy,
            policy,
            action,
        } = route;

        match host {
            Some(host) => {
                self.begin_table_field("host");
                self.http_host_selector(host);
                self.end_table();
            }
            None => self.nil_field("host"),
        }
        self.begin_table_field("path");
        self.http_path_selector(path);
        self.end_table();
        self.string_list_field("methods", methods);
        match access_policy {
            Some(policy) => {
                self.begin_table_field("access_policy");
                self.http_access_policy(service, route_index, policy)?;
                self.end_table();
            }
            None => self.nil_field("access_policy"),
        }
        self.begin_table_field("policy");
        self.http_route_policy(policy);
        self.end_table();
        self.begin_table_field("action");
        self.http_route_action(service, route_index, action)?;
        self.end_table();
        Ok(())
    }

    fn http_route_policy(&mut self, policy: &HttpRoutePolicy) {
        match policy.max_request_body_bytes {
            Some(limit) => self.integer_field("max_request_body_bytes", limit),
            None => self.null_field("max_request_body_bytes"),
        }
        self.integer_field("connect_timeout_ms", policy.connect_timeout_ms);
        self.integer_field("read_timeout_ms", policy.read_timeout_ms);
        self.integer_field("write_timeout_ms", policy.write_timeout_ms);
        self.boolean_field("request_buffering", policy.request_buffering);
        self.boolean_field("response_buffering", policy.response_buffering);
    }

    fn http_host_selector(&mut self, selector: &HttpHostSelector) {
        match selector {
            HttpHostSelector::NormalizedHost { value } => {
                self.string_field("kind", "normalized_host");
                self.string_field("value", value);
            }
            HttpHostSelector::ExactAuthority { value } => {
                self.string_field("kind", "exact_authority");
                self.string_field("value", value);
            }
            HttpHostSelector::AsciiCaseInsensitiveExactAuthority { value } => {
                self.string_field("kind", "ascii_case_insensitive_exact_authority");
                self.string_field("value", value);
            }
            HttpHostSelector::NginxLeadingWildcard { value } => {
                self.string_field("kind", "nginx_leading_wildcard");
                self.string_field("value", value);
            }
            HttpHostSelector::NginxLeadingDot { value } => {
                self.string_field("kind", "nginx_leading_dot");
                self.string_field("value", value);
            }
        }
    }

    fn http_path_selector(&mut self, selector: &HttpPathSelector) {
        let (kind, value) = match selector {
            HttpPathSelector::SegmentPrefix { value } => ("segment_prefix", value),
            HttpPathSelector::RawPrefix { value } => ("raw_prefix", value),
            HttpPathSelector::Exact { value } => ("exact", value),
            HttpPathSelector::AsciiCaseInsensitiveExact { value } => {
                ("ascii_case_insensitive_exact", value)
            }
        };
        self.string_field("kind", kind);
        self.string_field("value", value);
    }

    fn http_access_policy(
        &mut self,
        service: &str,
        route_index: usize,
        policy: &HttpAccessPolicy,
    ) -> Result<(), ConfigError> {
        match policy {
            HttpAccessPolicy::BearerTokenFile {
                token_file_path,
                header_name,
                realm,
            } => {
                self.string_field("type", "bearer_token_file");
                self.string_field(
                    "token_file_path",
                    utf8_http_route_path(
                        token_file_path,
                        service,
                        route_index,
                        "access_policy.token_file_path",
                    )?,
                );
                self.string_field("header_name", header_name);
                self.optional_string_field("realm", realm.as_deref());
            }
            HttpAccessPolicy::BasicHtpasswdFile {
                htpasswd_file_path,
                realm,
            } => {
                self.string_field("type", "basic_htpasswd_file");
                self.string_field(
                    "htpasswd_file_path",
                    utf8_http_route_path(
                        htpasswd_file_path,
                        service,
                        route_index,
                        "access_policy.htpasswd_file_path",
                    )?,
                );
                self.string_field("realm", realm);
            }
        }
        Ok(())
    }

    fn http_route_action(
        &mut self,
        service: &str,
        route_index: usize,
        action: &HttpRouteAction,
    ) -> Result<(), ConfigError> {
        match action {
            HttpRouteAction::Proxy {
                upstream_pool,
                policy,
            } => {
                self.string_field("type", "proxy");
                self.string_field("upstream_pool", upstream_pool);
                self.begin_table_field("policy");
                self.http_proxy_policy(service, route_index, policy)?;
                self.end_table();
            }
            HttpRouteAction::FixedResponse {
                status,
                body,
                headers,
            } => {
                self.string_field("type", "fixed_response");
                self.integer_field("status", status);
                self.string_field("body", body);
                self.table_list_or_nil_field("headers", headers, Self::http_literal_header);
            }
            HttpRouteAction::Redirect {
                status,
                location,
                headers,
            } => {
                self.string_field("type", "redirect");
                self.integer_field("status", status);
                self.begin_table_field("location");
                self.http_redirect_location(location);
                self.end_table();
                self.table_list_or_nil_field("headers", headers, Self::http_literal_header);
            }
            action @ HttpRouteAction::StaticFiles { .. } => {
                self.http_static_action(service, route_index, action)?;
            }
        }
        Ok(())
    }

    fn http_static_action(
        &mut self,
        service: &str,
        route_index: usize,
        action: &HttpRouteAction,
    ) -> Result<(), ConfigError> {
        let HttpRouteAction::StaticFiles {
            root_directory,
            path_mapping,
            index_files,
            internal_index_redirects,
            directory_redirects,
            spa_fallback,
            try_files,
            autoindex,
            autoindex_exact_size,
            autoindex_local_time,
            etag,
            mime,
            headers,
            error_responses,
        } = action
        else {
            unreachable!("static action renderer requires a static action");
        };
        self.string_field("type", "static_files");
        self.string_field(
            "root_directory",
            utf8_http_route_path(
                root_directory,
                service,
                route_index,
                "action.static_files.root_directory",
            )?,
        );
        self.string_field(
            "path_mapping",
            match path_mapping {
                HttpStaticPathMapping::Root => "root",
                HttpStaticPathMapping::Alias => "alias",
            },
        );
        self.string_list_field("index_files", index_files);
        self.boolean_field("internal_index_redirects", *internal_index_redirects);
        self.boolean_field("directory_redirects", *directory_redirects);
        match spa_fallback {
            Some(path) => self.string_field(
                "spa_fallback",
                utf8_http_route_path(
                    path,
                    service,
                    route_index,
                    "action.static_files.spa_fallback",
                )?,
            ),
            None => self.nil_field("spa_fallback"),
        }
        if try_files.is_empty() {
            self.nil_field("try_files");
        } else {
            self.fallible_table_list_field("try_files", try_files, |renderer, candidate| {
                renderer.http_static_try_file(service, route_index, candidate)
            })?;
        }
        self.boolean_field("autoindex", *autoindex);
        self.boolean_field("autoindex_exact_size", *autoindex_exact_size);
        self.boolean_field("autoindex_local_time", *autoindex_local_time);
        self.boolean_field("etag", *etag);
        self.begin_table_field("mime");
        self.http_static_mime(mime);
        self.end_table();
        self.table_list_or_nil_field("headers", headers, Self::http_literal_header);
        if error_responses.is_empty() {
            self.nil_field("error_responses");
        } else {
            self.fallible_table_list_field(
                "error_responses",
                error_responses,
                |renderer, response| {
                    renderer.http_static_error_response(service, route_index, response)
                },
            )?;
        }
        Ok(())
    }

    fn http_static_try_file(
        &mut self,
        service: &str,
        route_index: usize,
        candidate: &HttpStaticTryFile,
    ) -> Result<(), ConfigError> {
        match candidate {
            HttpStaticTryFile::RequestPath => self.string_field("type", "request_path"),
            HttpStaticTryFile::RequestPathDirectory => {
                self.string_field("type", "request_path_directory");
            }
            HttpStaticTryFile::Relative { path } => {
                self.string_field("type", "relative");
                self.string_field(
                    "path",
                    utf8_http_route_path(
                        path,
                        service,
                        route_index,
                        "action.static_files.try_files[].path",
                    )?,
                );
            }
            HttpStaticTryFile::Status { status } => {
                self.string_field("type", "status");
                self.integer_field("status", status);
            }
        }
        Ok(())
    }

    fn http_static_mime(&mut self, mime: &HttpStaticMimePolicy) {
        self.optional_string_field("default_type", mime.default_type.as_deref());
        self.table_list_or_nil_field("types", &mime.types, Self::http_mime_type);
    }

    fn http_mime_type(&mut self, mime: &HttpMimeType) {
        self.string_field("extension", &mime.extension);
        self.string_field("content_type", &mime.content_type);
    }

    fn http_static_error_response(
        &mut self,
        service: &str,
        route_index: usize,
        response: &HttpStaticErrorResponse,
    ) -> Result<(), ConfigError> {
        self.integer_list_field("statuses", &response.statuses);
        match &response.file {
            Some(file) => self.string_field(
                "file",
                utf8_http_route_path(
                    file,
                    service,
                    route_index,
                    "action.static_files.error_responses[].file",
                )?,
            ),
            None => self.null_field("file"),
        }
        self.optional_string_field("body", response.body.as_deref());
        self.table_list_or_nil_field("headers", &response.headers, Self::http_literal_header);
        self.optional_string_field("internal_redirect", response.internal_redirect.as_deref());
        Ok(())
    }

    fn http_proxy_policy(
        &mut self,
        service: &str,
        route_index: usize,
        policy: &HttpProxyPolicy,
    ) -> Result<(), ConfigError> {
        let HttpProxyPolicy {
            upstream_host,
            upstream_path_rewrite,
            request_headers,
            response_headers,
            response_cookie_path_rewrites,
            response_cookie_attributes,
            retry,
            cache,
        } = policy;
        self.begin_table_field("upstream_host");
        self.http_upstream_host(upstream_host);
        self.end_table();
        match upstream_path_rewrite {
            Some(rewrite) => {
                self.begin_table_field("upstream_path_rewrite");
                self.http_proxy_path_rewrite(rewrite);
                self.end_table();
            }
            None => self.nil_field("upstream_path_rewrite"),
        }
        self.table_list_or_nil_field(
            "request_headers",
            request_headers,
            Self::http_request_header_mutation,
        );
        self.table_list_or_nil_field(
            "response_cookie_attributes",
            response_cookie_attributes,
            Self::http_cookie_attribute,
        );
        self.table_list_or_nil_field(
            "response_headers",
            response_headers,
            Self::http_response_header_mutation,
        );
        self.table_list_or_nil_field(
            "response_cookie_path_rewrites",
            response_cookie_path_rewrites,
            Self::http_cookie_path_rewrite,
        );
        self.begin_table_field("retry");
        self.http_retry_policy(retry);
        self.end_table();
        match cache {
            Some(cache) => {
                self.begin_table_field("cache");
                self.http_cache_policy(service, route_index, cache)?;
                self.end_table();
            }
            None => self.nil_field("cache"),
        }
        Ok(())
    }

    fn http_cookie_attribute(&mut self, policy: &HttpCookieAttributePolicy) {
        self.string_field("name", &policy.name);
        self.optional_boolean_field("secure", policy.secure);
        self.optional_boolean_field("http_only", policy.http_only);
        match policy.same_site {
            Some(HttpSameSite::Strict) => self.string_field("same_site", "strict"),
            Some(HttpSameSite::Lax) => self.string_field("same_site", "lax"),
            Some(HttpSameSite::None) => self.string_field("same_site", "none"),
            None => self.nil_field("same_site"),
        }
    }

    fn http_proxy_path_rewrite(&mut self, rewrite: &HttpProxyPathRewrite) {
        self.string_field("from", &rewrite.from);
        self.string_field("to", &rewrite.to);
    }

    fn http_cache_policy(
        &mut self,
        service: &str,
        route_index: usize,
        cache: &HttpCachePolicy,
    ) -> Result<(), ConfigError> {
        self.string_field("store", &cache.store);
        self.string_list_field("methods", &cache.methods);
        self.table_list_field(
            "key_components",
            &cache.key_components,
            Self::cache_key_component,
        );
        self.boolean_field("use_origin_cache_control", cache.use_origin_cache_control);
        self.integer_field("default_ttl_ms", cache.default_ttl_ms);
        self.table_list_field("status_ttls", &cache.status_ttls, Self::cache_status_ttl);
        self.integer_field("grace_ms", cache.grace_ms);
        self.integer_field("keep_ms", cache.keep_ms);
        self.boolean_field("revalidate", cache.revalidate);
        self.boolean_field("collapsed_forwarding", cache.collapsed_forwarding);
        self.string_list_field(
            "stale_on",
            &cache
                .stale_on
                .iter()
                .map(|trigger| match trigger {
                    CacheStaleTrigger::ConnectFailure => "connect_failure",
                    CacheStaleTrigger::ConnectTimeout => "connect_timeout",
                    CacheStaleTrigger::Origin500 => "origin_500",
                    CacheStaleTrigger::Origin502 => "origin_502",
                    CacheStaleTrigger::Origin503 => "origin_503",
                    CacheStaleTrigger::Origin504 => "origin_504",
                })
                .collect::<Vec<_>>(),
        );
        self.table_list_field(
            "bypass_request",
            &cache.bypass_request,
            Self::cache_predicate,
        );
        self.table_list_field(
            "no_store_request",
            &cache.no_store_request,
            Self::cache_predicate,
        );
        self.table_list_field(
            "no_store_response",
            &cache.no_store_response,
            Self::cache_predicate,
        );
        self.string_field(
            "set_cookie_policy",
            match cache.set_cookie_policy {
                CacheSetCookiePolicy::Bypass => "bypass",
                CacheSetCookiePolicy::Ignore => "ignore",
            },
        );
        self.string_field(
            "authorization_policy",
            match cache.authorization_policy {
                CacheAuthorizationPolicy::Bypass => "bypass",
                CacheAuthorizationPolicy::Cache => "cache",
            },
        );
        self.string_field(
            "vary_policy",
            match cache.vary_policy {
                CacheVaryPolicy::Respect => "respect",
                CacheVaryPolicy::Ignore => "ignore",
            },
        );
        self.optional_table_field(
            "surrogate_tags",
            cache.surrogate_tags.as_ref(),
            Self::cache_surrogate_tags,
        );
        match &cache.purge_authorization {
            Some(CachePurgeAuthorization::BearerTokenFile { token_file_path }) => {
                self.begin_table_field("purge_authorization");
                self.string_field("type", "bearer_token_file");
                self.string_field(
                    "token_file_path",
                    utf8_http_route_path(
                        token_file_path,
                        service,
                        route_index,
                        "action.policy.cache.purge_authorization.token_file_path",
                    )?,
                );
                self.end_table();
            }
            None => self.nil_field("purge_authorization"),
        }
        Ok(())
    }

    fn cache_key_component(&mut self, component: &CacheKeyComponent) {
        match component {
            CacheKeyComponent::Scheme => self.string_field("type", "scheme"),
            CacheKeyComponent::NormalizedHost => self.string_field("type", "normalized_host"),
            CacheKeyComponent::PathAndQuery => self.string_field("type", "path_and_query"),
            CacheKeyComponent::Header { name } => {
                self.string_field("type", "header");
                self.string_field("name", name);
            }
            CacheKeyComponent::Cookie { name } => {
                self.string_field("type", "cookie");
                self.string_field("name", name);
            }
        }
    }

    fn cache_status_ttl(&mut self, status_ttl: &CacheStatusTtl) {
        self.integer_field("status", status_ttl.status);
        self.integer_field("ttl_ms", status_ttl.ttl_ms);
    }

    fn cache_predicate(&mut self, predicate: &CachePredicate) {
        match predicate {
            CachePredicate::HeaderPresent { name } => {
                self.string_field("type", "header_present");
                self.string_field("name", name);
            }
            CachePredicate::CookiePresent { name } => {
                self.string_field("type", "cookie_present");
                self.string_field("name", name);
            }
        }
    }

    fn cache_surrogate_tags(&mut self, tags: &CacheSurrogateTags) {
        self.string_field("response_header", &tags.response_header);
        self.integer_field("max_tags", tags.max_tags);
        self.integer_field("max_tag_bytes", tags.max_tag_bytes);
    }

    fn http_upstream_host(&mut self, policy: &HttpUpstreamHost) {
        match policy {
            HttpUpstreamHost::PreserveIncoming => {
                self.string_field("type", "preserve_incoming");
            }
            HttpUpstreamHost::NginxHost { fallback } => {
                self.string_field("type", "nginx_host");
                self.string_field("fallback", fallback);
            }
            HttpUpstreamHost::Endpoint { unix_fallback } => {
                self.string_field("type", "endpoint");
                self.optional_string_field("unix_fallback", unix_fallback.as_deref());
            }
            HttpUpstreamHost::Literal { value } => {
                self.string_field("type", "literal");
                self.string_field("value", value);
            }
        }
    }

    fn http_request_header_mutation(&mut self, mutation: &HttpRequestHeaderMutation) {
        match mutation {
            HttpRequestHeaderMutation::Set { name, value } => {
                self.string_field("operation", "set");
                self.string_field("name", name);
                self.begin_table_field("value");
                self.http_request_header_value(value);
                self.end_table();
            }
            HttpRequestHeaderMutation::Remove { name } => {
                self.string_field("operation", "remove");
                self.string_field("name", name);
            }
        }
    }

    fn http_request_header_value(&mut self, value: &HttpRequestHeaderValue) {
        match value {
            HttpRequestHeaderValue::Literal { value } => {
                self.string_field("type", "literal");
                self.string_field("value", value);
            }
            HttpRequestHeaderValue::IncomingAuthority => {
                self.string_field("type", "incoming_authority");
            }
            HttpRequestHeaderValue::NormalizedHost => {
                self.string_field("type", "normalized_host");
            }
            HttpRequestHeaderValue::NginxHost { fallback } => {
                self.string_field("type", "nginx_host");
                self.string_field("fallback", fallback);
            }
            HttpRequestHeaderValue::ClientIp => self.string_field("type", "client_ip"),
            HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes,
                except_source_cidrs,
            } => {
                self.string_field("type", "appended_x_forwarded_for");
                self.integer_field("max_bytes", max_bytes);
                if !except_source_cidrs.is_empty() {
                    self.string_list_field("except_source_cidrs", except_source_cidrs);
                }
            }
            HttpRequestHeaderValue::DownstreamScheme => {
                self.string_field("type", "downstream_scheme");
            }
            HttpRequestHeaderValue::IncomingHeader { name, max_bytes } => {
                self.string_field("type", "incoming_header");
                self.string_field("name", name);
                self.integer_field("max_bytes", max_bytes);
            }
            HttpRequestHeaderValue::SelectedUpstreamHost => {
                self.string_field("type", "selected_upstream_host");
            }
        }
    }

    fn http_response_header_mutation(&mut self, mutation: &HttpResponseHeaderMutation) {
        match mutation {
            HttpResponseHeaderMutation::Set {
                name,
                value,
                always,
            } => {
                self.string_field("operation", "set");
                self.string_field("name", name);
                self.string_field("value", value);
                self.boolean_field("always", *always);
            }
            HttpResponseHeaderMutation::Add {
                name,
                value,
                always,
            } => {
                self.string_field("operation", "add");
                self.string_field("name", name);
                self.string_field("value", value);
                self.boolean_field("always", *always);
            }
            HttpResponseHeaderMutation::Remove { name } => {
                self.string_field("operation", "remove");
                self.string_field("name", name);
            }
        }
    }

    fn http_cookie_path_rewrite(&mut self, rewrite: &HttpCookiePathRewrite) {
        self.string_field("from", &rewrite.from);
        self.string_field("to", &rewrite.to);
    }

    fn http_retry_policy(&mut self, retry: &HttpRetryPolicy) {
        self.integer_field("max_retries", retry.max_retries);
        self.string_field(
            "target",
            match retry.target {
                HttpRetryTarget::SameServer => "same_server",
                HttpRetryTarget::NextServer => "next_server",
            },
        );
        self.integer_field("delay_ms", retry.delay_ms);
        self.boolean_field("final_redispatch", retry.final_redispatch);
        self.string_list_field(
            "triggers",
            &retry
                .triggers
                .iter()
                .map(|trigger| match trigger {
                    HttpRetryTrigger::ConnectFailure => "connect_failure",
                    HttpRetryTrigger::ConnectTimeout => "connect_timeout",
                    HttpRetryTrigger::RefusedStream => "refused_stream",
                })
                .collect::<Vec<_>>(),
        );
        self.string_field(
            "method_safety",
            match retry.method_safety {
                HttpRetryMethodSafety::GetHead => "get_head",
            },
        );
        self.string_field(
            "body_safety",
            match retry.body_safety {
                HttpRetryBodySafety::Empty => "empty",
            },
        );
    }

    fn http_literal_header(&mut self, header: &HttpLiteralHeader) {
        self.string_field("name", &header.name);
        self.string_field("value", &header.value);
        self.boolean_field("always", header.always);
    }

    fn http_redirect_location(&mut self, location: &HttpRedirectLocation) {
        match location {
            HttpRedirectLocation::Literal { value } => {
                self.string_field("kind", "literal");
                self.string_field("value", value);
            }
            HttpRedirectLocation::RequestTemplate {
                value,
                nginx_host_fallback,
            } => {
                self.string_field("kind", "request_template");
                self.string_field("value", value);
                self.optional_string_field("nginx_host_fallback", nginx_host_fallback.as_deref());
            }
        }
    }

    fn forward_proxy_service(&mut self, service: &ForwardProxyService) -> Result<(), ConfigError> {
        self.string_field("name", &service.name);
        self.string_list_field(
            "enabled_versions",
            &service
                .enabled_versions
                .iter()
                .map(|version| match version {
                    ForwardHttpVersion::H1 => "h1",
                    ForwardHttpVersion::H2 => "h2",
                    ForwardHttpVersion::H3 => "h3",
                })
                .collect::<Vec<_>>(),
        );
        self.boolean_field("allow_absolute_form", service.allow_absolute_form);
        self.boolean_field("tls_required", service.tls_required);
        self.begin_table_field("connect");
        self.forward_connect_policy(&service.connect);
        self.end_table();
        self.begin_table_field("peer_policy");
        self.forward_peer_policy(&service.peer_policy);
        self.end_table();
        self.forward_proxy_auth(service)?;
        match &service.access_policy {
            Some(policy) => {
                self.begin_table_field("access_policy");
                self.forward_access_policy(policy);
                self.end_table();
            }
            None => self.nil_field("access_policy"),
        }
        self.begin_table_field("destination_policy");
        self.forward_destination_policy(&service.destination_policy);
        self.end_table();
        self.begin_table_field("header_policy");
        self.forward_header_policy(&service.header_policy);
        self.end_table();
        match service.header_policy.cache.as_deref() {
            Some(cache) => {
                self.begin_table_field("cache");
                self.http_cache_policy(&service.name, 0, cache)?;
                self.end_table();
            }
            None => self.nil_field("cache"),
        }
        self.integer_field("connect_timeout_ms", service.connect_timeout_ms);
        self.integer_field("idle_timeout_ms", service.idle_timeout_ms);
        self.integer_field("lifetime_timeout_ms", service.lifetime_timeout_ms);
        match service.max_request_body_bytes {
            Some(value) => self.integer_field("max_request_body_bytes", value),
            None => self.null_field("max_request_body_bytes"),
        }
        self.integer_field("max_header_bytes", service.max_header_bytes);
        self.integer_field("max_connections", service.max_connections);
        self.begin_table_field("resolver");
        self.forward_resolver_policy(&service.resolver);
        self.end_table();
        self.string_field(
            "audit_mode",
            match service.audit_mode {
                ForwardAuditMode::Off => "off",
                ForwardAuditMode::Metadata => "metadata",
            },
        );
        Ok(())
    }

    fn forward_proxy_auth(&mut self, service: &ForwardProxyService) -> Result<(), ConfigError> {
        match &service.auth {
            Some(ForwardProxyAuth::BearerTokenFile { token_file_path }) => {
                self.begin_table_field("auth");
                self.string_field("type", "bearer_token_file");
                self.string_field(
                    "token_file_path",
                    utf8_path(
                        token_file_path,
                        "forward proxy service",
                        &service.name,
                        "auth.token_file_path",
                    )?,
                );
                self.end_table();
            }
            Some(ForwardProxyAuth::BasicHtpasswdFile {
                htpasswd_file_path,
                realm,
                credential_ttl_ms,
                username_case_sensitive,
            }) => {
                self.begin_table_field("auth");
                self.string_field("type", "basic_htpasswd_file");
                self.string_field(
                    "htpasswd_file_path",
                    utf8_path(
                        htpasswd_file_path,
                        "forward proxy service",
                        &service.name,
                        "auth.htpasswd_file_path",
                    )?,
                );
                self.string_field("realm", realm);
                match credential_ttl_ms {
                    Some(value) => self.integer_field("credential_ttl_ms", *value),
                    None => self.nil_field("credential_ttl_ms"),
                }
                self.boolean_field("username_case_sensitive", *username_case_sensitive);
                self.end_table();
            }
            Some(ForwardProxyAuth::MutualTls {
                client_ca_file_path,
            }) => {
                self.begin_table_field("auth");
                self.string_field("type", "mutual_tls");
                self.string_field(
                    "client_ca_file_path",
                    utf8_path(
                        client_ca_file_path,
                        "forward proxy service",
                        &service.name,
                        "auth.client_ca_file_path",
                    )?,
                );
                self.end_table();
            }
            None => self.nil_field("auth"),
        }
        Ok(())
    }

    fn forward_connect_policy(&mut self, policy: &ForwardConnectPolicy) {
        self.boolean_field("enabled", policy.enabled);
        self.integer_list_field("allowed_ports", &policy.allowed_ports);
    }

    fn forward_peer_policy(&mut self, policy: &ForwardPeerPolicy) {
        self.begin_table_field("peers");
        for peer in &policy.peers {
            self.begin_table_item();
            self.string_field("host", &peer.host);
            self.integer_field("port", u64::from(peer.port));
            self.end_table();
        }
        self.end_table();
        self.string_field(
            "direct_fallback",
            match policy.direct_fallback {
                ForwardDirectFallback::Allowed => "allowed",
                ForwardDirectFallback::Denied => "denied",
                ForwardDirectFallback::Required => "required",
            },
        );
        self.integer_field("max_retries", u64::from(policy.max_retries));
    }

    fn forward_destination_policy(&mut self, policy: &ForwardDestinationPolicy) {
        self.string_list_field("allow_domains", &policy.allow_domains);
        self.string_list_field("deny_domains", &policy.deny_domains);
        self.string_list_field("allow_cidrs", &policy.allow_cidrs);
        self.string_list_field("deny_cidrs", &policy.deny_cidrs);
        self.boolean_field("deny_private", policy.deny_private);
        self.forward_time_ranges("allow_times", &policy.allow_times);
        self.forward_time_ranges("deny_times", &policy.deny_times);
    }

    fn forward_time_ranges(&mut self, name: &str, ranges: &[ForwardTimeRange]) {
        self.begin_table_field(name);
        for range in ranges {
            self.begin_table_item();
            self.string_list_field(
                "days",
                &range
                    .days
                    .iter()
                    .map(|day| match day {
                        ForwardWeekday::Monday => "monday",
                        ForwardWeekday::Tuesday => "tuesday",
                        ForwardWeekday::Wednesday => "wednesday",
                        ForwardWeekday::Thursday => "thursday",
                        ForwardWeekday::Friday => "friday",
                        ForwardWeekday::Saturday => "saturday",
                        ForwardWeekday::Sunday => "sunday",
                    })
                    .collect::<Vec<_>>(),
            );
            self.string_field("start", &range.start);
            self.string_field("end", &range.end);
            self.end_table();
        }
        self.end_table();
    }

    fn forward_access_policy(&mut self, policy: &ForwardAccessPolicy) {
        self.begin_table_field("rules");
        for rule in &policy.rules {
            self.begin_table_item();
            self.forward_access_rule(rule);
            self.end_table();
        }
        self.end_table();
        self.string_field(
            "default_action",
            forward_access_action(policy.default_action),
        );
    }

    fn forward_access_rule(&mut self, rule: &ForwardAccessRule) {
        self.string_field("action", forward_access_action(rule.action));
        self.begin_table_field("conditions");
        for condition in &rule.conditions {
            self.begin_table_item();
            self.boolean_field("negated", condition.negated);
            match &condition.matcher {
                ForwardAccessMatcher::All => self.string_field("type", "all"),
                ForwardAccessMatcher::Methods { methods } => {
                    self.string_field("type", "methods");
                    self.string_list_field("methods", methods);
                }
                ForwardAccessMatcher::SourceCidrs { cidrs } => {
                    self.string_field("type", "source_cidrs");
                    self.string_list_field("cidrs", cidrs);
                }
                ForwardAccessMatcher::DestinationPorts { ranges } => {
                    self.string_field("type", "destination_ports");
                    self.begin_table_field("ranges");
                    for range in ranges {
                        self.begin_table_item();
                        self.integer_field("start", u64::from(range.start));
                        self.integer_field("end", u64::from(range.end));
                        self.end_table();
                    }
                    self.end_table();
                }
                ForwardAccessMatcher::Authenticated => {
                    self.string_field("type", "authenticated");
                }
                ForwardAccessMatcher::DestinationLocal => {
                    self.string_field("type", "destination_local");
                }
                ForwardAccessMatcher::DestinationLinkLocal => {
                    self.string_field("type", "destination_link_local");
                }
                ForwardAccessMatcher::Manager => self.string_field("type", "manager"),
            }
            self.end_table();
        }
        self.end_table();
    }

    fn forward_header_policy(&mut self, policy: &ForwardHeaderPolicy) {
        self.string_field(
            "forwarded_for",
            match policy.forwarded_for {
                ForwardedForPolicy::Preserve => "preserve",
                ForwardedForPolicy::Delete => "delete",
            },
        );
        self.string_field(
            "via",
            match policy.via {
                ForwardViaPolicy::Preserve => "preserve",
                ForwardViaPolicy::Delete => "delete",
            },
        );
    }

    fn forward_resolver_policy(&mut self, policy: &ForwardResolverPolicy) {
        self.string_list_field(
            "nameservers",
            &policy
                .nameservers
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        );
        self.integer_field("max_cache_entries", policy.max_cache_entries);
        self.integer_field("max_concurrent_queries", policy.max_concurrent_queries);
        self.integer_field("max_addresses_per_name", policy.max_addresses_per_name);
        self.integer_field("min_ttl_ms", policy.min_ttl_ms);
        self.integer_field("max_ttl_ms", policy.max_ttl_ms);
        self.integer_field("negative_ttl_ms", policy.negative_ttl_ms);
        self.boolean_field("revalidate_on_connect", policy.revalidate_on_connect);
    }

    fn l4_service(&mut self, service: &L4Service) {
        let L4Service {
            name,
            upstream_pool,
            connect_timeout_ms,
            idle_timeout_ms,
            lifetime_timeout_ms,
            proxy_protocol,
            udp,
        } = service;

        self.string_field("name", name);
        self.string_field("upstream_pool", upstream_pool);
        self.integer_field("connect_timeout_ms", connect_timeout_ms);
        self.integer_field("idle_timeout_ms", idle_timeout_ms);
        match lifetime_timeout_ms {
            Some(timeout) => self.integer_field("lifetime_timeout_ms", timeout),
            None => self.nil_field("lifetime_timeout_ms"),
        }
        self.optional_table_field(
            "proxy_protocol",
            proxy_protocol.as_ref(),
            Self::proxy_protocol,
        );
        self.optional_table_field("udp", udp.as_ref(), Self::udp_policy);
    }

    fn udp_policy(&mut self, policy: &UdpPolicy) {
        self.integer_field("max_datagram_bytes", policy.max_datagram_bytes);
        self.integer_field("max_sessions", policy.max_sessions);
        self.integer_field("max_session_bytes", policy.max_session_bytes);
        self.integer_field("max_queue_datagrams", policy.max_queue_datagrams);
        self.integer_field("max_queue_bytes", policy.max_queue_bytes);
    }

    fn table_list_field<T>(&mut self, name: &str, values: &[T], render: fn(&mut Self, &T)) {
        self.begin_table_field(name);
        for value in values {
            self.begin_table_item();
            render(self, value);
            self.end_table();
        }
        self.end_table();
    }

    fn table_list_or_nil_field<T>(&mut self, name: &str, values: &[T], render: fn(&mut Self, &T)) {
        if values.is_empty() {
            self.nil_field(name);
        } else {
            self.table_list_field(name, values, render);
        }
    }

    fn fallible_table_list_field<T, F>(
        &mut self,
        name: &str,
        values: &[T],
        mut render: F,
    ) -> Result<(), ConfigError>
    where
        F: FnMut(&mut Self, &T) -> Result<(), ConfigError>,
    {
        self.begin_table_field(name);
        for value in values {
            self.begin_table_item();
            render(self, value)?;
            self.end_table();
        }
        self.end_table();
        Ok(())
    }

    fn optional_table_field<T>(
        &mut self,
        name: &str,
        value: Option<&T>,
        render: fn(&mut Self, &T),
    ) {
        match value {
            Some(value) => {
                self.begin_table_field(name);
                render(self, value);
                self.end_table();
            }
            None => self.nil_field(name),
        }
    }

    fn fallible_optional_table_field<T>(
        &mut self,
        name: &str,
        value: Option<&T>,
        render: fn(&mut Self, &T) -> Result<(), ConfigError>,
    ) -> Result<(), ConfigError> {
        match value {
            Some(value) => {
                self.begin_table_field(name);
                render(self, value)?;
                self.end_table();
            }
            None => self.nil_field(name),
        }
        Ok(())
    }

    fn begin_table_field(&mut self, name: &str) {
        self.indent();
        push_lua_field_name(&mut self.output, name);
        self.output.push_str(" = {\n");
        self.indent += 1;
    }

    fn begin_table_item(&mut self) {
        self.indent();
        self.output.push_str("{\n");
        self.indent += 1;
    }

    fn end_table(&mut self) {
        self.indent -= 1;
        self.indent();
        self.output.push_str("},\n");
    }

    fn string_field(&mut self, name: &str, value: &str) {
        self.field_name(name);
        push_lua_string(&mut self.output, value);
        self.output.push_str(",\n");
    }

    fn optional_string_field(&mut self, name: &str, value: Option<&str>) {
        match value {
            Some(value) => self.string_field(name, value),
            None => self.nil_field(name),
        }
    }

    fn optional_integer_field<T: Display>(&mut self, name: &str, value: Option<T>) {
        match value {
            Some(value) => self.integer_field(name, value),
            None => self.nil_field(name),
        }
    }

    fn optional_boolean_field(&mut self, name: &str, value: Option<bool>) {
        match value {
            Some(value) => self.boolean_field(name, value),
            None => self.nil_field(name),
        }
    }

    fn access_log_field(
        &mut self,
        field: &str,
        policy: Option<&AccessLogPolicy>,
        kind: &'static str,
        name: &str,
    ) -> Result<(), ConfigError> {
        match policy {
            Some(AccessLogPolicy::Disabled) => {
                self.begin_table_field(field);
                self.string_field("type", "disabled");
                self.end_table();
            }
            Some(AccessLogPolicy::File { path }) => {
                self.begin_table_field(field);
                self.string_field("type", "file");
                self.string_field("path", utf8_path(path, kind, name, "access_log.path")?);
                self.end_table();
            }
            None => self.nil_field(field),
        }
        Ok(())
    }

    fn string_list_field<S>(&mut self, name: &str, values: &[S])
    where
        S: AsRef<str>,
    {
        self.field_name(name);
        self.output.push('{');
        for (index, value) in values.iter().enumerate() {
            if index == 0 {
                self.output.push(' ');
            } else {
                self.output.push_str(", ");
            }
            push_lua_string(&mut self.output, value.as_ref());
        }
        if !values.is_empty() {
            self.output.push(' ');
        }
        self.output.push_str("},\n");
    }

    fn integer_list_field<T: Display>(&mut self, name: &str, values: &[T]) {
        self.field_name(name);
        self.output.push('{');
        for (index, value) in values.iter().enumerate() {
            if index == 0 {
                self.output.push(' ');
            } else {
                self.output.push_str(", ");
            }
            write!(self.output, "{value}").expect("writing to String cannot fail");
        }
        if !values.is_empty() {
            self.output.push(' ');
        }
        self.output.push_str("},\n");
    }

    fn integer_field(&mut self, name: &str, value: impl Display) {
        self.field_name(name);
        write!(self.output, "{value}").expect("writing to String cannot fail");
        self.output.push_str(",\n");
    }

    fn boolean_field(&mut self, name: &str, value: bool) {
        self.field_name(name);
        self.output.push_str(if value { "true" } else { "false" });
        self.output.push_str(",\n");
    }

    fn nil_field(&mut self, name: &str) {
        self.field_name(name);
        self.output.push_str("nil,\n");
    }

    fn null_field(&mut self, name: &str) {
        self.field_name(name);
        self.output.push_str("null,\n");
    }

    fn field_name(&mut self, name: &str) {
        self.indent();
        push_lua_field_name(&mut self.output, name);
        self.output.push_str(" = ");
    }

    fn indent(&mut self) {
        for _ in 0..self.indent {
            self.output.push_str("  ");
        }
    }
}

fn push_lua_field_name(output: &mut String, name: &str) {
    const KEYWORDS: &[&str] = &[
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if",
        "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
    ];
    let mut characters = name.chars();
    let identifier = characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric());
    if identifier && !KEYWORDS.contains(&name) {
        output.push_str(name);
    } else {
        output.push('[');
        push_lua_string(output, name);
        output.push(']');
    }
}

fn utf8_path<'a>(
    path: &'a Path,
    kind: &'static str,
    name: &str,
    field: &'static str,
) -> Result<&'a str, ConfigError> {
    path.to_str().ok_or_else(|| ConfigError::InvalidFilePath {
        kind,
        name: name.into(),
        field,
        detail: "path must be valid UTF-8",
    })
}

const fn forward_access_action(action: ForwardAccessAction) -> &'static str {
    match action {
        ForwardAccessAction::Allow => "allow",
        ForwardAccessAction::Deny => "deny",
    }
}

fn utf8_recording_root<'a>(
    path: &'a Path,
    service: &str,
    application: &str,
    recorder: &str,
) -> Result<&'a str, ConfigError> {
    path.to_str()
        .ok_or_else(|| ConfigError::InvalidRtmpRecorderPolicy {
            service: service.into(),
            application: application.into(),
            recorder: recorder.into(),
            field: "root_directory",
            detail: "path must be valid UTF-8",
        })
}

fn utf8_http_route_path<'a>(
    path: &'a Path,
    service: &str,
    route: usize,
    field: &'static str,
) -> Result<&'a str, ConfigError> {
    path.to_str().ok_or_else(|| ConfigError::InvalidHttpRoute {
        service: service.into(),
        route,
        field,
        detail: "path must be valid UTF-8".into(),
    })
}

fn http_version(version: HttpVersion) -> &'static str {
    match version {
        HttpVersion::Http11 => "1.1",
        HttpVersion::Http2 => "2",
        HttpVersion::Http3 => "3",
    }
}

fn push_lua_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                write!(output, "\\u{{{:x}}}", u32::from(character))
                    .expect("writing to String cannot fail");
            }
            character => output.push(character),
        }
    }
    output.push('"');
}
