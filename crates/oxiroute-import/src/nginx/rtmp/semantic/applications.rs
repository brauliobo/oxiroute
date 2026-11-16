#[allow(clippy::wildcard_imports)]
use super::*;

impl Resolver<'_> {
    #[allow(clippy::too_many_lines)]
    pub(super) fn resolve_application(
        &mut self,
        directive: &ExpandedDirective,
        inherited: &Policy,
    ) -> EffectiveRtmpApplication {
        self.resolve_block_header(directive, DirectiveContext::RtmpServer, "application");
        let name = directive
            .directive
            .arguments
            .first()
            .map(|word| self.value(word));
        if name
            .as_ref()
            .is_some_and(|name| !valid_canonical_name(&name.value))
        {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "nginx-RTMP application name is not a valid canonical name",
            );
        }

        let children = directive.children.as_deref().unwrap_or_default();
        let policy = self.resolve_local_policy(
            children,
            DirectiveContext::RtmpApplication,
            inherited.clone(),
        );
        let mut recorders = Vec::new();
        let mut push_targets = Vec::new();
        let default_recorder_name = format!("nginx-recorder-{}", directive.occurrence.get());
        let mut recorder_names = HashMap::new();
        if let Some(recorder) = self.effective_recorder(
            directive,
            &policy,
            default_recorder_name.clone(),
            Self::origin(directive),
        ) {
            recorder_names.insert(default_recorder_name, directive.occurrence);
            recorders.push(recorder);
        }
        for child in children {
            if child.directive.name.value == b"recorder" {
                if let Some(name) = child
                    .directive
                    .arguments
                    .first()
                    .and_then(|name| std::str::from_utf8(&name.value).ok())
                    && let Some(first) = recorder_names.insert(name.to_owned(), child.occurrence)
                {
                    self.block_related(
                        child.occurrence,
                        E_DUPLICATE_IDENTITY,
                        "duplicate nginx-RTMP recorder name in one application",
                        first,
                    );
                }
                if let Some(recorder) = self.resolve_recorder(child, &policy) {
                    recorders.push(recorder);
                }
            } else if child.directive.name.value == b"push" {
                if self
                    .validate_registered(child, DirectiveContext::RtmpApplication)
                    .is_ok()
                    && child.directive.children.is_none()
                    && child.directive.arguments.len() == 1
                {
                    if let Some((host, port, application)) =
                        parse_push_target(&child.directive.arguments[0].value)
                    {
                        push_targets.push(EffectiveRtmpPushTarget {
                            host,
                            port,
                            application,
                            origin: Self::origin(child),
                        });
                        self.resolved(child.occurrence);
                    } else {
                        self.block(
                            child.occurrence,
                            E_INVALID_VALUE,
                            "push must be an rtmp:// host, optional port, and one literal or $name application",
                        );
                    }
                }
            } else if child.directive.name.value == b"hls_muxdelay" {
                self.resolve_source_noop(child, DirectiveContext::RtmpApplication);
            } else if !is_supported_policy(&child.directive.name.value) {
                self.resolve_unsupported_subtree(
                    child,
                    DirectiveContext::RtmpApplication,
                    unsupported_rtmp_reason(&child.directive.name.value),
                );
            }
        }

        let exec_profiles = policy
            .exec_profiles
            .iter()
            .cloned()
            .map(|mut profile| {
                profile.respawn = profile.respawn && policy.respawn.value;
                profile.respawn_origin = profile
                    .respawn
                    .then(|| policy.respawn.origin.clone())
                    .flatten();
                profile.respawn_delay_ms = policy.respawn_timeout_ms.value;
                profile
                    .respawn_delay_origin
                    .clone_from(&policy.respawn_timeout_ms.origin);
                profile
            })
            .collect();
        EffectiveRtmpApplication {
            origin: Self::origin(directive),
            name,
            push_targets,
            policy: EffectiveRtmpPolicy {
                live: policy.live.value,
                live_origin: policy.live.origin,
                idle_streams: policy.idle_streams.value,
                idle_streams_origin: policy.idle_streams.origin,
                publish_access: policy.publish_access.clone(),
                play_access: policy.play_access.clone(),
                max_connections: policy.max_connections.value,
                max_connections_origin: policy.max_connections.origin,
                hls: self.finish_hls_policy(&policy.hls, directive.occurrence),
                exec_profiles,
                recorders,
            },
        }
    }

    fn finish_hls_policy(
        &mut self,
        policy: &HlsPolicy,
        occurrence: OccurrenceId,
    ) -> Option<RtmpHlsPolicy> {
        if !policy.enabled.value {
            return None;
        }
        let Some(root_directory) = policy.root_directory.value.clone() else {
            self.block(
                occurrence,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "hls on requires an absolute hls_path",
            );
            return None;
        };
        Some(RtmpHlsPolicy {
            root_directory,
            segment_duration_ms: policy.segment_duration_ms.value,
            max_segment_duration_ms: policy.max_segment_duration_ms.value,
            playlist_length_ms: policy.playlist_length_ms.value,
            fragment_naming: policy.fragment_naming.value,
            nested: policy.nested.value,
            cleanup: policy.cleanup.value,
            variants: Vec::new(),
            keys: policy.keys.value.clone(),
            max_segment_bytes: 8 * 1024 * 1024,
            max_queue_messages: 256,
            max_storage_bytes: 512 * 1024 * 1024,
            max_storage_files: 10_000,
            max_active_streams: 1_024,
        })
    }

    fn resolve_recorder(
        &mut self,
        directive: &ExpandedDirective,
        inherited: &Policy,
    ) -> Option<EffectiveRtmpRecorder> {
        self.resolve_block_header(directive, DirectiveContext::RtmpApplication, "recorder");
        let name = directive
            .directive
            .arguments
            .first()
            .map(|name| self.value(name));
        let name = name?;
        if !valid_canonical_name(&name.value) {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "nginx-RTMP recorder name is not a valid canonical name",
            );
            return None;
        }
        let name = std::str::from_utf8(&name.value)
            .expect("validated recorder name is UTF-8")
            .to_owned();
        let children = directive.children.as_deref().unwrap_or_default();
        let policy =
            self.resolve_local_policy(children, DirectiveContext::RtmpRecorder, inherited.clone());
        for child in children {
            if !is_supported_policy(&child.directive.name.value) {
                self.resolve_unsupported_subtree(
                    child,
                    DirectiveContext::RtmpRecorder,
                    "nginx-RTMP recorder callback or file policy has no canonical recorder equivalent",
                );
            }
        }
        self.effective_recorder(directive, &policy, name, Self::origin(directive))
    }

    fn effective_recorder(
        &mut self,
        application: &ExpandedDirective,
        policy: &Policy,
        name: String,
        name_origin: DirectiveOrigin,
    ) -> Option<EffectiveRtmpRecorder> {
        let mode = match policy.record.value {
            RecordSetting::Off => return None,
            RecordSetting::Continuous => RtmpRecordMode::Continuous,
            RecordSetting::Manual => RtmpRecordMode::Manual,
        };
        let record_origin = policy
            .record
            .origin
            .clone()
            .expect("enabled record policy has a directive origin");
        let Some(root_directory) = policy.path.value.clone() else {
            self.block(
                record_origin.occurrence,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "enabled nginx-RTMP recording requires a secure absolute record_path",
            );
            return None;
        };
        if !policy.live.value {
            self.block(
                record_origin.occurrence,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "canonical recorders require live on for the nginx-RTMP application",
            );
            return None;
        }
        if mode == RtmpRecordMode::Manual && policy.interval.value.is_some() {
            let interval_origin = policy
                .interval
                .origin
                .as_ref()
                .expect("explicit record interval has an origin");
            self.block(
                interval_origin.occurrence,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "nginx ignores record_interval for manual recording",
            );
            return None;
        }
        let path_origin = policy
            .path
            .origin
            .clone()
            .unwrap_or_else(|| Self::origin(application));
        Some(EffectiveRtmpRecorder {
            name,
            name_origin,
            mode,
            record_origin,
            record_mask: policy.record_mask.value,
            mask_origin: policy
                .record_mask
                .origin
                .clone()
                .unwrap_or_else(|| policy.record.origin.clone().expect("record origin")),
            append: policy.append.value,
            append_origin: policy.append.origin.clone(),
            lock: policy.lock.value,
            lock_origin: policy.lock.origin.clone(),
            max_size: policy.max_size.value,
            max_size_origin: policy.max_size.origin.clone(),
            max_frames: policy.max_frames.value,
            max_frames_origin: policy.max_frames.origin.clone(),
            notify: policy.notify.value,
            notify_origin: policy.notify.origin.clone(),
            root_directory,
            path_origin,
            suffix_template: policy.suffix.value.clone(),
            suffix_origin: policy.suffix.origin.clone(),
            append_unix_seconds: policy.unique.value,
            unique_origin: policy.unique.origin.clone(),
            rotation_interval_ms: policy.interval.value,
            interval_origin: policy.interval.origin.clone(),
        })
    }
}
