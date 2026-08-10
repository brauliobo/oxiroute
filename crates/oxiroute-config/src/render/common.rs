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
        let common = store.common();
        match store.kind() {
            CacheStoreKind::Memory => {
                self.string_field("type", "memory");
                self.string_field("name", common.name);
                self.integer_field("max_bytes", common.max_bytes);
                self.integer_field("max_entries", common.max_entries);
            }
            CacheStoreKind::Disk => {
                self.string_field("type", "disk");
                self.string_field("name", common.name);
                let root = store
                    .root_directory()
                    .expect("disk cache store has a root directory")
                    .to_str()
                    .ok_or_else(|| ConfigError::InvalidCacheStore {
                        store: common.name.into(),
                        field: "root_directory",
                        detail: "path must be valid UTF-8".into(),
                    })?;
                self.string_field("root_directory", root);
                self.integer_field("max_bytes", common.max_bytes);
                self.integer_field("max_files", common.max_entries);
            }
        }
        self.cache_store_common(common);
        Ok(())
    }

    fn cache_store_common(&mut self, common: CacheStoreCommon<'_>) {
        self.integer_field("max_object_bytes", common.max_object_bytes);
        self.integer_field("max_header_bytes", common.max_header_bytes);
        self.integer_field("max_key_bytes", common.max_key_bytes);
        self.integer_field("max_tag_bytes", common.max_tag_bytes);
        self.integer_field("max_tags_per_object", common.max_tags_per_object);
        self.integer_field("max_in_flight_fills", common.max_in_flight_fills);
        self.integer_field("max_followers_per_fill", common.max_followers_per_fill);
    }
}
