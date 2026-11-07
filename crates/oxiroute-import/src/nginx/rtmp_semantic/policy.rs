#[allow(clippy::wildcard_imports)]
use super::*;

impl Resolver<'_> {
    pub(super) fn resolve_local_policy(
        &mut self,
        children: &[ExpandedDirective],
        context: DirectiveContext,
        mut policy: Policy,
    ) -> Policy {
        let inherited_publish_access = std::mem::take(&mut policy.publish_access);
        let inherited_play_access = std::mem::take(&mut policy.play_access);
        let mut seen = HashMap::new();
        for child in children {
            let name = child.directive.name.value.as_slice();
            if !is_supported_policy(name) {
                continue;
            }
            let repeatable = matches!(
                name,
                b"allow"
                    | b"deny"
                    | b"exec"
                    | b"exec_push"
                    | b"exec_publish"
                    | b"exec_publish_done"
            );
            if !repeatable && let Some(first) = seen.insert(name.to_vec(), child.occurrence) {
                self.block_related(
                    child.occurrence,
                    E_DUPLICATE_IDENTITY,
                    "duplicate nginx-RTMP scalar directive in one context",
                    first,
                );
                continue;
            }
            if self.validate_registered(child, context).is_err()
                || child.directive.children.is_some()
            {
                if child.directive.children.is_some() {
                    self.block(
                        child.occurrence,
                        E_INVALID_VALUE,
                        "nginx-RTMP scalar directive must end with a semicolon",
                    );
                }
                continue;
            }
            self.resolved(child.occurrence);
            let origin = Self::origin(child);
            self.apply_policy(child, name, origin, context, &mut policy);
        }
        policy.publish_access.extend(inherited_publish_access);
        policy.play_access.extend(inherited_play_access);
        policy
    }

    #[allow(clippy::too_many_lines)]
    fn apply_policy(
        &mut self,
        child: &ExpandedDirective,
        name: &[u8],
        origin: DirectiveOrigin,
        context: DirectiveContext,
        policy: &mut Policy,
    ) {
        let argument = &child.directive.arguments[0].value;
        match name {
            b"live" => policy.live.replace(argument == b"on", origin),
            b"idle_streams" => policy.idle_streams.replace(argument == b"on", origin),
            b"allow" | b"deny" => {
                let Some(rule) = parse_access_rule(child, name, origin) else {
                    self.block(
                        child.occurrence,
                        E_INVALID_VALUE,
                        "nginx-RTMP access rule arguments must be valid UTF-8",
                    );
                    return;
                };
                if matches!(rule.operation, AccessOperation::Publish | AccessOperation::Both) {
                    policy.publish_access.push(rule.clone());
                }
                if matches!(rule.operation, AccessOperation::Play | AccessOperation::Both) {
                    policy.play_access.push(rule);
                }
            }
            b"max_connections" => {
                self.apply_max_connections(child, argument, origin, context, policy);
            }
            b"exec" | b"exec_push" | b"exec_publish" | b"exec_publish_done" => {
                self.apply_exec(child, name, origin, policy);
            }
            b"respawn" => policy.respawn.replace(argument == b"on", origin),
            b"respawn_timeout" => match parse_nginx_milliseconds(argument) {
                Some(value) if (1..=MAX_EXEC_RESPAWN_TIMEOUT_MS).contains(&value) => {
                    policy.respawn_timeout_ms.replace(value, origin);
                }
                _ => self.block(
                    child.occurrence,
                    E_INVALID_VALUE,
                    "respawn_timeout is outside canonical millisecond bounds",
                ),
            },
            b"rtmp_auto_push" => match argument.as_slice() {
                b"on" => policy.auto_push.replace(true, origin),
                b"off" => policy.auto_push.replace(false, origin),
                _ => self.block(
                    child.occurrence,
                    E_INVALID_VALUE,
                    "rtmp_auto_push must be on or off",
                ),
            },
            b"rtmp_auto_push_reconnect" => match parse_nginx_milliseconds(argument) {
                Some(value) if (1..=300_000).contains(&value) => {
                    policy.auto_push_reconnect_ms.replace(value, origin);
                }
                _ => self.block(
                    child.occurrence,
                    E_INVALID_VALUE,
                    "rtmp_auto_push_reconnect is outside canonical millisecond bounds",
                ),
            },
            b"rtmp_socket_dir" => match secure_auto_push_root(argument) {
                Some(path) => policy.auto_push_socket_dir.replace(path, origin),
                None => self.block(
                    child.occurrence,
                    E_INVALID_VALUE,
                    "rtmp_socket_dir must be a secure absolute UTF-8 directory path",
                ),
            },
            b"record" => match parse_record(&child.directive.arguments) {
                Ok((value, mask)) => {
                    policy.record.replace(value, origin.clone());
                    policy.record_mask.replace(mask, origin);
                }
                Err(message) => self.block(child.occurrence, E_SEMANTICS_NOT_REPRESENTABLE, message),
            },
            b"record_path" => match secure_recording_root(argument) {
                Some(path) => policy.path.replace(Some(path), origin),
                None => self.block(
                    child.occurrence,
                    E_INVALID_VALUE,
                    "record_path must be secure absolute UTF-8 directory syntax",
                ),
            },
            b"record_suffix" => match exact_suffix(argument) {
                Some(suffix) => policy.suffix.replace(suffix, origin),
                None => self.block(
                    child.occurrence,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "record_suffix is outside the exact canonical grammar or uses local-time formatting",
                ),
            },
            b"record_unique" => policy.unique.replace(argument == b"on", origin),
            b"record_interval" => match parse_nginx_milliseconds(argument) {
                Some(interval) if (1..=MAX_ROTATION_INTERVAL_MS).contains(&interval) => {
                    policy.interval.replace(Some(interval), origin);
                }
                _ => self.block(
                    child.occurrence,
                    E_INVALID_VALUE,
                    "record_interval is outside canonical millisecond bounds",
                ),
            },
            b"record_append" => policy.append.replace(argument == b"on", origin),
            b"record_lock" => policy.lock.replace(argument == b"on", origin),
            b"record_max_size" => match parse_nginx_size(argument) {
                Some(0) => policy.max_size.replace(None, origin),
                Some(value) if value <= MAX_RECORDING_FILE_BYTES => {
                    policy.max_size.replace(Some(value), origin);
                }
                _ => self.block(
                    child.occurrence,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "record_max_size exceeds the bounded canonical per-file limit",
                ),
            },
            b"record_max_frames" => match parse_nginx_size(argument) {
                Some(0) => policy.max_frames.replace(None, origin),
                Some(value) if value <= MAX_RECORDING_FRAME_COUNT => {
                    policy.max_frames.replace(Some(value), origin);
                }
                _ => self.block(
                    child.occurrence,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "record_max_frames exceeds the bounded canonical per-file limit",
                ),
            },
            b"record_notify" => policy.notify.replace(argument == b"on", origin),
            b"hls" => policy.hls.enabled.replace(argument == b"on", origin),
            b"hls_path" => match secure_recording_root(argument) {
                Some(path) => policy.hls.root_directory.replace(Some(path), origin),
                None => self.block(
                    child.occurrence,
                    E_INVALID_VALUE,
                    "hls_path must be secure absolute UTF-8 directory syntax",
                ),
            },
            b"hls_fragment" => match parse_nginx_milliseconds(argument) {
                Some(value) if (1..=MAX_HLS_DURATION_MS).contains(&value) => {
                    policy.hls.segment_duration_ms.replace(value, origin);
                }
                _ => self.block(
                    child.occurrence,
                    E_INVALID_VALUE,
                    "hls_fragment is outside canonical millisecond bounds",
                ),
            },
            b"hls_max_fragment" => match parse_nginx_milliseconds(argument) {
                Some(value) if (1..=MAX_HLS_DURATION_MS).contains(&value) => {
                    policy.hls.max_segment_duration_ms.replace(value, origin);
                }
                _ => self.block(
                    child.occurrence,
                    E_INVALID_VALUE,
                    "hls_max_fragment is outside canonical millisecond bounds",
                ),
            },
            b"hls_playlist_length" => match parse_nginx_milliseconds(argument) {
                Some(value) if (1..=MAX_HLS_PLAYLIST_LENGTH_MS).contains(&value) => {
                    policy.hls.playlist_length_ms.replace(value, origin);
                }
                _ => self.block(
                    child.occurrence,
                    E_INVALID_VALUE,
                    "hls_playlist_length is outside canonical millisecond bounds",
                ),
            },
            b"hls_nested" => policy.hls.nested.replace(argument == b"on", origin),
            b"hls_cleanup" => policy.hls.cleanup.replace(argument == b"on", origin),
            b"hls_fragment_naming" => {
                let naming = match argument.as_slice() {
                    b"sequential" => Some(RtmpHlsFragmentNaming::Sequential),
                    b"timestamp" => Some(RtmpHlsFragmentNaming::Timestamp),
                    b"system" => Some(RtmpHlsFragmentNaming::System),
                    _ => None,
                };
                if let Some(naming) = naming {
                    policy.hls.fragment_naming.replace(naming, origin);
                } else {
                    self.block(
                        child.occurrence,
                        E_INVALID_VALUE,
                        "hls_fragment_naming is not a supported canonical value",
                    );
                }
            }
            b"hls_keys" => {
                if argument == b"on" {
                    let current_url_prefix = policy
                        .hls
                        .keys
                        .value
                        .as_ref()
                        .map_or_else(String::new, |keys| keys.url_prefix.clone());
                    policy.hls.keys.replace(
                        Some(RtmpHlsKeyPolicy {
                            rotation_segments: 5,
                            url_prefix: current_url_prefix,
                        }),
                        origin,
                    );
                } else {
                    policy.hls.keys.replace(None, origin);
                }
            }
            b"hls_key_url" => match std::str::from_utf8(argument) {
                Ok(value) if value.is_ascii() => {
                    let mut keys = policy.hls.keys.value.clone().unwrap_or(RtmpHlsKeyPolicy {
                        rotation_segments: 5,
                        url_prefix: String::new(),
                    });
                    value.clone_into(&mut keys.url_prefix);
                    policy.hls.keys.replace(Some(keys), origin);
                }
                _ => self.block(
                    child.occurrence,
                    E_INVALID_VALUE,
                    "hls_key_url must be ASCII",
                ),
            },
            b"hls_fragments_per_key" => match parse_u64(argument) {
                Some(value) if value <= 100_000 => {
                    let mut keys = policy.hls.keys.value.clone().unwrap_or(RtmpHlsKeyPolicy {
                        rotation_segments: 5,
                        url_prefix: String::new(),
                    });
                    keys.rotation_segments = value.max(1);
                    policy.hls.keys.replace(Some(keys), origin);
                }
                _ => self.block(
                    child.occurrence,
                    E_INVALID_VALUE,
                    "hls_fragments_per_key is outside canonical bounds",
                ),
            },
            _ => unreachable!("supported policy name was matched"),
        }
    }

    fn apply_exec(
        &mut self,
        child: &ExpandedDirective,
        name: &[u8],
        origin: DirectiveOrigin,
        policy: &mut Policy,
    ) {
        let Some(executable) = child.directive.arguments.first() else {
            return;
        };
        let Ok(executable) = std::str::from_utf8(&executable.value) else {
            self.block(
                child.occurrence,
                E_INVALID_VALUE,
                "exec executable must be UTF-8",
            );
            return;
        };
        if !valid_exec_path(executable) {
            self.block(
                child.occurrence,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "exec requires one bounded absolute executable path without traversal",
            );
            return;
        }
        let Some(arguments) = child
            .directive
            .arguments
            .iter()
            .skip(1)
            .map(|argument| std::str::from_utf8(&argument.value).ok())
            .collect::<Option<Vec<_>>>()
        else {
            self.block(
                child.occurrence,
                E_INVALID_VALUE,
                "exec arguments must be UTF-8",
            );
            return;
        };
        if arguments.iter().any(|argument| {
            argument
                .bytes()
                .any(|byte| byte == 0 || byte.is_ascii_control())
                || argument.bytes().any(|byte| matches!(byte, b'<' | b'>'))
        }) {
            self.block(
                child.occurrence,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "exec redirection and control tokens are not represented by typed argv",
            );
            return;
        }
        let (mode, trigger) = match name {
            b"exec" | b"exec_push" | b"exec_publish" => {
                (RtmpExecMode::Command, RtmpExecTrigger::Publisher)
            }
            b"exec_publish_done" => (RtmpExecMode::Command, RtmpExecTrigger::PublishDone),
            _ => unreachable!("exec directive was matched before lowering"),
        };
        let managed = matches!(name, b"exec" | b"exec_push");
        policy.exec_profiles.push(EffectiveRtmpExecProfile {
            name: format!("nginx-exec-{}", child.occurrence.get()),
            origin,
            mode,
            trigger,
            executable: PathBuf::from(executable),
            arguments: arguments.into_iter().map(str::to_owned).collect(),
            respawn: managed && policy.respawn.value,
            respawn_origin: managed.then(|| policy.respawn.origin.clone()).flatten(),
            respawn_delay_ms: policy.respawn_timeout_ms.value,
            respawn_delay_origin: policy.respawn_timeout_ms.origin.clone(),
        });
    }

    fn apply_max_connections(
        &mut self,
        child: &ExpandedDirective,
        argument: &[u8],
        origin: DirectiveOrigin,
        context: DirectiveContext,
        policy: &mut Policy,
    ) {
        if context != DirectiveContext::RtmpApplication {
            self.block(
                child.occurrence,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "nginx-RTMP max_connections is lowered only at application scope",
            );
            return;
        }
        match parse_u64(argument) {
            Some(value) if (1..=MAX_APPLICATION_CONNECTIONS).contains(&value) => {
                policy.max_connections.replace(Some(value), origin);
            }
            _ => self.block(
                child.occurrence,
                E_INVALID_VALUE,
                "application max_connections is outside canonical RTMP bounds",
            ),
        }
    }
}
