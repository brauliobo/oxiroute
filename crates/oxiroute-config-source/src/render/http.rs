impl Renderer {
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
                oxiroute_config::HttpGzipMinimumVersion::Http10 => "1.0",
                oxiroute_config::HttpGzipMinimumVersion::Http11 => "1.1",
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
        self.table_list_or_nil_field("status_ttls", &cache.status_ttls, Self::cache_status_ttl);
        self.integer_field("grace_ms", cache.grace_ms);
        self.integer_field("keep_ms", cache.keep_ms);
        self.boolean_field("revalidate", cache.revalidate);
        self.boolean_field("collapsed_forwarding", cache.collapsed_forwarding);
        if cache.stale_on.is_empty() {
            self.nil_field("stale_on");
        } else {
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
        }
        self.table_list_or_nil_field(
            "bypass_request",
            &cache.bypass_request,
            Self::cache_predicate,
        );
        self.table_list_or_nil_field(
            "no_store_request",
            &cache.no_store_request,
            Self::cache_predicate,
        );
        self.table_list_or_nil_field(
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
                    HttpRetryTrigger::EmptyResponse => "empty_response",
                    HttpRetryTrigger::ResponseTimeout => "response_timeout",
                    HttpRetryTrigger::JunkResponse => "junk_response",
                })
                .collect::<Vec<_>>(),
        );
        if !retry.response_statuses.is_empty() {
            self.integer_list_field("response_statuses", &retry.response_statuses);
        }
        self.string_field(
            "method_safety",
            match retry.method_safety {
                HttpRetryMethodSafety::GetHead => "get_head",
                HttpRetryMethodSafety::All => "all",
            },
        );
        self.string_field(
            "body_safety",
            match retry.body_safety {
                HttpRetryBodySafety::Empty => "empty",
                HttpRetryBodySafety::Buffered => "buffered",
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
}
