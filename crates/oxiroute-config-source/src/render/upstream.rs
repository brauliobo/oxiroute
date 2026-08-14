impl Renderer {
    fn upstream_pool(&mut self, pool: &UpstreamPool) -> Result<(), ConfigError> {
        let UpstreamPool {
            name,
            servers,
            endpoints,
            algorithm,
            health_check,
            passive_health,
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
        self.optional_table_field(
            "passive_health",
            passive_health.as_ref(),
            Self::passive_health,
        );
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

    fn passive_health(&mut self, policy: &PassiveHealthPolicy) {
        self.string_field(
            "observe",
            match policy.observe {
                PassiveObserve::Layer4 => "layer4",
                PassiveObserve::Layer7 => "layer7",
            },
        );
        self.string_field(
            "on_error",
            match policy.on_error {
                PassiveOnError::Count => "count",
                PassiveOnError::Immediately => "immediately",
                PassiveOnError::MarkDown => "mark_down",
            },
        );
        self.integer_field("error_limit", policy.error_limit);
        self.boolean_field("mark_down", policy.mark_down);
        self.boolean_field("mark_up", policy.mark_up);
        self.integer_field("initial_backoff_ms", policy.initial_backoff_ms);
        self.integer_field("max_backoff_ms", policy.max_backoff_ms);
        self.integer_field("recovery_threshold", policy.recovery_threshold);
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
}
