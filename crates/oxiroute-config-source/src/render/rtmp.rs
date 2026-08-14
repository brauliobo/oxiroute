impl Renderer {
    fn rtmp_service(&mut self, service: &RtmpService) -> Result<(), ConfigError> {
        let RtmpService {
            name,
            outbound_chunk_size,
            max_inbound_message_size,
            ack_window_size,
            access_log,
            outbound_policy,
            callbacks,
            auto_push,
            exec_profiles,
            applications,
        } = service;
        self.string_field("name", name);
        self.integer_field("outbound_chunk_size", outbound_chunk_size);
        self.integer_field("max_inbound_message_size", max_inbound_message_size);
        self.integer_field("ack_window_size", ack_window_size);
        self.access_log_field("access_log", access_log.as_ref(), "RTMP service", name)?;
        self.begin_table_field("outbound_policy");
        self.rtmp_outbound_policy(outbound_policy);
        self.end_table();
        self.begin_table_field("callbacks");
        self.rtmp_callbacks(callbacks);
        self.end_table();
        self.begin_table_field("auto_push");
        self.rtmp_auto_push(auto_push)?;
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
        self.table_list_field(
            "environment",
            &profile.environment,
            Self::rtmp_exec_environment,
        );
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

    fn rtmp_auto_push(
        &mut self,
        policy: &oxiroute_config::RtmpAutoPushPolicy,
    ) -> Result<(), ConfigError> {
        self.boolean_field("enabled", policy.enabled);
        self.string_field(
            "socket_dir",
            utf8_path(
                &policy.socket_dir,
                "RTMP auto-push",
                "auto_push",
                "socket_dir",
            )?,
        );
        match &policy.secret_file {
            Some(path) => self.string_field(
                "secret_file",
                utf8_path(path, "RTMP auto-push", "auto_push", "secret_file")?,
            ),
            None => self.null_field("secret_file"),
        }
        self.integer_field("reconnect_ms", policy.reconnect_ms);
        self.integer_field("connect_timeout_ms", policy.connect_timeout_ms);
        self.integer_field("handshake_timeout_ms", policy.handshake_timeout_ms);
        self.integer_field("max_peers", policy.max_peers);
        self.integer_field("max_queue_messages", policy.max_queue_messages);
        self.integer_field("max_queue_bytes", policy.max_queue_bytes);
        self.integer_field("max_streams", policy.max_streams);
        Ok(())
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
            utf8_path(
                &policy.root_directory,
                "RTMP DASH",
                "dash",
                "root_directory",
            )?,
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

    fn rtmp_credentials(&mut self, credentials: &oxiroute_config::RtmpCredentialReference) {
        self.string_field("username", &credentials.username);
        self.string_field(
            "secret_file",
            credentials
                .secret_file
                .to_str()
                .expect("validated RTMP credential path is UTF-8"),
        );
    }

    fn rtmp_outbound_policy(&mut self, policy: &oxiroute_config::RtmpOutboundPolicy) {
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
        self.integer_field("dns_refresh_ms", policy.dns_refresh_ms);
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
}
