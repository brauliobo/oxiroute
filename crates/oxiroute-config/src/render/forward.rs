impl Renderer {
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
        self.begin_table_field("connect_udp");
        self.forward_connect_policy(&service.connect_udp);
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
}
