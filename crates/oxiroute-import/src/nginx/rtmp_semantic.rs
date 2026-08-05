use std::{collections::HashMap, net::SocketAddr, path::PathBuf};

use oxiroute_config::{
    RtmpAclAction, RtmpExecMode, RtmpExecTrigger, RtmpHlsFragmentNaming, RtmpHlsKeyPolicy,
    RtmpHlsPolicy, RtmpRecordMask,
};
use oxiroute_rtmp::{DirectiveContext, DirectiveError, validate_directive};

use crate::{
    Diagnostic, DiagnosticCode, DiagnosticStage, E_DUPLICATE_IDENTITY, E_INCLUDE_CYCLE,
    E_INVALID_VALUE, E_SEMANTICS_NOT_REPRESENTABLE, E_SOURCE_CHANGED, E_SOURCE_IO, E_SOURCE_LIMIT,
    E_UNSUPPORTED_FEATURE, Report, Severity,
};

use super::{
    DirectiveOrigin, ExpandedDirective, ExpandedOccurrence, IncludeCandidateStatus, NginxValue,
    OccurrenceDecision, OccurrenceDisposition, OccurrenceId, SourceGraph, Word,
};

const MAX_RECORDING_ROOT_BYTES: usize = 4_096;
const MAX_SUFFIX_BYTES: usize = 128;
const MAX_ROTATION_INTERVAL_MS: u64 = (1 << 31) - 1;
const MAX_OUTBOUND_CHUNK_SIZE: u32 = 1_048_576;
const MAX_APPLICATION_CONNECTIONS: u64 = 100_000;
const MAX_RECORDING_FILE_BYTES: u64 = 1_099_511_627_776;
const MAX_RECORDING_FRAME_COUNT: u64 = 1_000_000_000;
const DEFAULT_HLS_SEGMENT_DURATION_MS: u64 = 2_000;
const DEFAULT_HLS_MAX_SEGMENT_DURATION_MS: u64 = 10_000;
const DEFAULT_HLS_PLAYLIST_LENGTH_MS: u64 = 30_000;
const MAX_HLS_DURATION_MS: u64 = 120_000;
const MAX_HLS_PLAYLIST_LENGTH_MS: u64 = 86_400_000;
const MAX_EXEC_RESPAWN_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_EXEC_RESPAWN_TIMEOUT_MS: u64 = 5_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpResolution {
    pub rtmp_blocks: Vec<EffectiveRtmp>,
    /// One entry for every bounded expanded occurrence, in expansion order.
    pub decisions: Vec<OccurrenceDecision>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRtmp {
    pub origin: DirectiveOrigin,
    pub outbound_chunk_size: u32,
    pub chunk_size_origin: Option<DirectiveOrigin>,
    pub auto_push: bool,
    pub auto_push_origin: Option<DirectiveOrigin>,
    pub auto_push_reconnect_ms: u64,
    pub auto_push_reconnect_origin: Option<DirectiveOrigin>,
    pub auto_push_socket_dir: PathBuf,
    pub auto_push_socket_origin: Option<DirectiveOrigin>,
    pub access_log_disabled: bool,
    pub access_log_path: Option<PathBuf>,
    pub access_log_origin: Option<DirectiveOrigin>,
    pub servers: Vec<EffectiveRtmpServer>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRtmpServer {
    pub origin: DirectiveOrigin,
    pub outbound_chunk_size: Option<u32>,
    pub chunk_size_origin: Option<DirectiveOrigin>,
    pub listens: Vec<EffectiveRtmpListen>,
    pub applications: Vec<EffectiveRtmpApplication>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRtmpListen {
    pub origin: DirectiveOrigin,
    pub value: Option<NginxValue>,
    pub address: Option<SocketAddr>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRtmpApplication {
    pub origin: DirectiveOrigin,
    pub name: Option<NginxValue>,
    pub policy: EffectiveRtmpPolicy,
    pub push_targets: Vec<EffectiveRtmpPushTarget>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRtmpPushTarget {
    pub host: String,
    pub port: u16,
    pub application: String,
    pub origin: DirectiveOrigin,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRtmpPolicy {
    pub live: bool,
    pub live_origin: Option<DirectiveOrigin>,
    pub idle_streams: bool,
    pub idle_streams_origin: Option<DirectiveOrigin>,
    pub publish_access: Vec<EffectiveRtmpAccessRule>,
    pub play_access: Vec<EffectiveRtmpAccessRule>,
    pub max_connections: Option<u64>,
    pub max_connections_origin: Option<DirectiveOrigin>,
    pub hls: Option<RtmpHlsPolicy>,
    pub exec_profiles: Vec<EffectiveRtmpExecProfile>,
    pub recorders: Vec<EffectiveRtmpRecorder>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRtmpExecProfile {
    pub name: String,
    pub origin: DirectiveOrigin,
    pub mode: RtmpExecMode,
    pub trigger: RtmpExecTrigger,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub respawn: bool,
    pub respawn_origin: Option<DirectiveOrigin>,
    pub respawn_delay_ms: u64,
    pub respawn_delay_origin: Option<DirectiveOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveRtmpAccessRule {
    pub action: RtmpAclAction,
    pub network: String,
    pub origin: DirectiveOrigin,
    operation: AccessOperation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AccessOperation {
    Publish,
    Play,
    Both,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct EffectiveRtmpRecorder {
    pub name: String,
    pub name_origin: DirectiveOrigin,
    pub mode: RtmpRecordMode,
    pub record_origin: DirectiveOrigin,
    pub record_mask: RtmpRecordMask,
    pub mask_origin: DirectiveOrigin,
    pub append: bool,
    pub append_origin: Option<DirectiveOrigin>,
    pub lock: bool,
    pub lock_origin: Option<DirectiveOrigin>,
    pub max_size: Option<u64>,
    pub max_size_origin: Option<DirectiveOrigin>,
    pub max_frames: Option<u64>,
    pub max_frames_origin: Option<DirectiveOrigin>,
    pub notify: bool,
    pub notify_origin: Option<DirectiveOrigin>,
    pub root_directory: PathBuf,
    pub path_origin: DirectiveOrigin,
    pub suffix_template: String,
    pub suffix_origin: Option<DirectiveOrigin>,
    pub append_unix_seconds: bool,
    pub unique_origin: Option<DirectiveOrigin>,
    pub rotation_interval_ms: Option<u64>,
    pub interval_origin: Option<DirectiveOrigin>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpRecordMode {
    Continuous,
    Manual,
}

#[derive(Clone, Debug)]
struct Policy {
    live: Setting<bool>,
    idle_streams: Setting<bool>,
    publish_access: Vec<EffectiveRtmpAccessRule>,
    play_access: Vec<EffectiveRtmpAccessRule>,
    max_connections: Setting<Option<u64>>,
    record: Setting<RecordSetting>,
    record_mask: Setting<RtmpRecordMask>,
    path: Setting<Option<PathBuf>>,
    suffix: Setting<String>,
    unique: Setting<bool>,
    append: Setting<bool>,
    lock: Setting<bool>,
    max_size: Setting<Option<u64>>,
    max_frames: Setting<Option<u64>>,
    notify: Setting<bool>,
    interval: Setting<Option<u64>>,
    exec_profiles: Vec<EffectiveRtmpExecProfile>,
    respawn: Setting<bool>,
    respawn_timeout_ms: Setting<u64>,
    auto_push: Setting<bool>,
    auto_push_reconnect_ms: Setting<u64>,
    auto_push_socket_dir: Setting<PathBuf>,
    hls: HlsPolicy,
}

#[derive(Clone, Debug)]
struct HlsPolicy {
    enabled: Setting<bool>,
    root_directory: Setting<Option<PathBuf>>,
    segment_duration_ms: Setting<u64>,
    max_segment_duration_ms: Setting<u64>,
    playlist_length_ms: Setting<u64>,
    fragment_naming: Setting<RtmpHlsFragmentNaming>,
    nested: Setting<bool>,
    cleanup: Setting<bool>,
    keys: Setting<Option<RtmpHlsKeyPolicy>>,
}

#[derive(Clone, Debug)]
struct Setting<T> {
    value: T,
    origin: Option<DirectiveOrigin>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordSetting {
    Off,
    Continuous,
    Manual,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            live: Setting::new(false),
            idle_streams: Setting::new(true),
            publish_access: Vec::new(),
            play_access: Vec::new(),
            max_connections: Setting::new(None),
            record: Setting::new(RecordSetting::Off),
            record_mask: Setting::new(RtmpRecordMask::default()),
            path: Setting::new(None),
            suffix: Setting::new(".flv".into()),
            unique: Setting::new(false),
            append: Setting::new(false),
            lock: Setting::new(false),
            max_size: Setting::new(None),
            max_frames: Setting::new(None),
            notify: Setting::new(false),
            interval: Setting::new(None),
            exec_profiles: Vec::new(),
            respawn: Setting::new(true),
            respawn_timeout_ms: Setting::new(DEFAULT_EXEC_RESPAWN_TIMEOUT_MS),
            auto_push: Setting::new(false),
            auto_push_reconnect_ms: Setting::new(100),
            auto_push_socket_dir: Setting::new(PathBuf::from("/tmp")),
            hls: HlsPolicy::default(),
        }
    }
}

impl Default for HlsPolicy {
    fn default() -> Self {
        Self {
            enabled: Setting::new(false),
            root_directory: Setting::new(None),
            segment_duration_ms: Setting::new(DEFAULT_HLS_SEGMENT_DURATION_MS),
            max_segment_duration_ms: Setting::new(DEFAULT_HLS_MAX_SEGMENT_DURATION_MS),
            playlist_length_ms: Setting::new(DEFAULT_HLS_PLAYLIST_LENGTH_MS),
            fragment_naming: Setting::new(RtmpHlsFragmentNaming::Sequential),
            nested: Setting::new(false),
            cleanup: Setting::new(true),
            keys: Setting::new(None),
        }
    }
}

impl<T> Setting<T> {
    const fn new(value: T) -> Self {
        Self {
            value,
            origin: None,
        }
    }

    fn replace(&mut self, value: T, origin: DirectiveOrigin) {
        self.value = value;
        self.origin = Some(origin);
    }
}

#[must_use]
pub fn resolve_rtmp(loaded: Report<SourceGraph>) -> Report<RtmpResolution> {
    let (graph, mut diagnostics) = loaded.into_parts();
    let (resolution, resolve_diagnostics) = resolve_rtmp_graph(&graph).into_parts();
    diagnostics.extend(resolve_diagnostics);
    Report::new(resolution, diagnostics)
}

pub(super) fn resolve_rtmp_graph(graph: &SourceGraph) -> Report<RtmpResolution> {
    Resolver::new(graph, false).run()
}

pub(super) fn resolve_rtmp_root_graph(graph: &SourceGraph) -> Report<RtmpResolution> {
    Resolver::new(graph, true).run()
}

struct Resolver<'a> {
    graph: &'a SourceGraph,
    dispositions: Vec<Option<OccurrenceDisposition>>,
    diagnostics: Vec<Diagnostic>,
    complete_root: bool,
}

impl<'a> Resolver<'a> {
    fn new(graph: &'a SourceGraph, complete_root: bool) -> Self {
        Self {
            graph,
            dispositions: vec![None; graph.expanded_occurrences.len()],
            diagnostics: Vec::new(),
            complete_root,
        }
    }

    fn run(mut self) -> Report<RtmpResolution> {
        let mut rtmp_blocks = Vec::new();
        let mut first_rtmp = None;
        let root_auto_push = self.resolve_root_auto_push_policy();

        for directive in &self.graph.expanded_directives {
            if directive.directive.name.value == b"rtmp" {
                if let Some(first) = first_rtmp {
                    self.block_related(
                        directive.occurrence,
                        E_DUPLICATE_IDENTITY,
                        "nginx permits only one effective rtmp block",
                        first,
                    );
                } else {
                    first_rtmp = Some(directive.occurrence);
                }
                rtmp_blocks.push(self.resolve_rtmp_block(directive, &root_auto_push));
            } else if is_root_auto_push_policy(&directive.directive.name.value) {
                // The exact nginx directives are resolved once at Nginx-main scope above.
            } else if self.complete_root {
                self.structural_subtree(directive);
            } else if Self::is_registered_in_context(directive, DirectiveContext::NginxMain) {
                self.resolve_unsupported_subtree(
                    directive,
                    DirectiveContext::NginxMain,
                    unsupported_rtmp_reason(&directive.directive.name.value),
                );
            } else {
                self.structural_subtree(directive);
            }
        }

        self.reject_overlapping_listens(&rtmp_blocks);
        self.classify_remaining();
        let decisions = self
            .graph
            .expanded_occurrences
            .iter()
            .map(|occurrence| self.decision(occurrence))
            .collect();

        Report::new(
            RtmpResolution {
                rtmp_blocks,
                decisions,
            },
            self.diagnostics,
        )
    }

    fn resolve_root_auto_push_policy(&mut self) -> Policy {
        let directives: Vec<_> = self
            .graph
            .expanded_directives
            .iter()
            .filter(|directive| is_root_auto_push_policy(&directive.directive.name.value))
            .cloned()
            .collect();
        self.resolve_local_policy(&directives, DirectiveContext::NginxMain, Policy::default())
    }

    #[allow(clippy::too_many_lines)]
    fn resolve_rtmp_block(
        &mut self,
        directive: &ExpandedDirective,
        root_auto_push: &Policy,
    ) -> EffectiveRtmp {
        self.resolve_block_header(directive, DirectiveContext::NginxMain, "rtmp");
        let children = directive.children.as_deref().unwrap_or_default();
        let mut policy =
            self.resolve_local_policy(children, DirectiveContext::RtmpMain, Policy::default());
        policy.auto_push = root_auto_push.auto_push.clone();
        policy.auto_push_reconnect_ms = root_auto_push.auto_push_reconnect_ms.clone();
        policy.auto_push_socket_dir = root_auto_push.auto_push_socket_dir.clone();
        let mut servers = Vec::new();
        let mut outbound_chunk_size = 4_096;
        let mut chunk_size_origin = None;
        let mut access_log_disabled = false;
        let mut access_log_path = None;
        let mut access_log_origin = None;

        for child in children {
            if child.directive.name.value == b"server" {
                let server = self.resolve_server(child, &policy);
                if let Some(server_chunk_size) = server.outbound_chunk_size {
                    if chunk_size_origin.is_some() && outbound_chunk_size != server_chunk_size {
                        self.block(
                            server
                                .chunk_size_origin
                                .as_ref()
                                .expect("server chunk size retains its origin")
                                .occurrence,
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            "nginx-RTMP servers use different chunk sizes but canonical policy is service-wide",
                        );
                    } else {
                        outbound_chunk_size = server_chunk_size;
                        chunk_size_origin.clone_from(&server.chunk_size_origin);
                    }
                }
                servers.push(server);
            } else if child.directive.name.value == b"chunk_size" {
                if self
                    .validate_registered(child, DirectiveContext::RtmpMain)
                    .is_ok()
                    && child.directive.children.is_none()
                {
                    match parse_u32(&child.directive.arguments[0].value) {
                        Some(value) if (1..=MAX_OUTBOUND_CHUNK_SIZE).contains(&value) => {
                            outbound_chunk_size = value;
                            chunk_size_origin = Some(Self::origin(child));
                            self.resolved(child.occurrence);
                        }
                        _ => self.block(
                            child.occurrence,
                            E_INVALID_VALUE,
                            "chunk_size is outside canonical RTMP outbound chunk bounds",
                        ),
                    }
                }
            } else if child.directive.name.value == b"access_log" {
                if self
                    .validate_registered(child, DirectiveContext::RtmpMain)
                    .is_ok()
                    && child.directive.children.is_none()
                    && (child.directive.arguments.len() == 1
                        || (child.directive.arguments.len() == 2
                            && child.directive.arguments[1].value == b"combined"))
                {
                    if child.directive.arguments.len() == 1
                        && child.directive.arguments[0].value == b"off"
                    {
                        access_log_disabled = true;
                        access_log_origin = Some(Self::origin(child));
                        self.resolved(child.occurrence);
                    } else if (child.directive.arguments.len() == 1
                        || (child.directive.arguments.len() == 2
                            && child.directive.arguments[1].value == b"combined"))
                        && std::str::from_utf8(&child.directive.arguments[0].value)
                            .is_ok_and(|path| path.starts_with('/'))
                    {
                        let path = std::str::from_utf8(&child.directive.arguments[0].value)
                            .expect("UTF-8 access_log path was checked above");
                        access_log_path = Some(PathBuf::from(path));
                        access_log_origin = Some(Self::origin(child));
                        self.resolved(child.occurrence);
                    } else {
                        self.block(
                            child.occurrence,
                            E_UNSUPPORTED_FEATURE,
                            "RTMP access_log requires an absolute path with the optional combined format",
                        );
                    }
                }
            } else if child.directive.name.value == b"hls_muxdelay" {
                self.resolve_source_noop(child, DirectiveContext::RtmpMain);
            } else if !is_supported_policy(&child.directive.name.value) {
                self.resolve_unsupported_subtree(
                    child,
                    DirectiveContext::RtmpMain,
                    unsupported_rtmp_reason(&child.directive.name.value),
                );
            }
        }

        if servers.is_empty() {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "rtmp requires at least one server block",
            );
        }
        let auto_push_socket_dir =
            if policy.auto_push.value && policy.auto_push_socket_dir.origin.is_none() {
                PathBuf::from("/tmp/oxiroute-rtmp")
            } else {
                policy.auto_push_socket_dir.value
            };
        EffectiveRtmp {
            origin: Self::origin(directive),
            outbound_chunk_size,
            chunk_size_origin,
            auto_push: policy.auto_push.value,
            auto_push_origin: policy.auto_push.origin,
            auto_push_reconnect_ms: policy.auto_push_reconnect_ms.value,
            auto_push_reconnect_origin: policy.auto_push_reconnect_ms.origin,
            auto_push_socket_dir,
            auto_push_socket_origin: policy.auto_push_socket_dir.origin,
            access_log_disabled,
            access_log_path,
            access_log_origin,
            servers,
        }
    }

    fn resolve_server(
        &mut self,
        directive: &ExpandedDirective,
        inherited: &Policy,
    ) -> EffectiveRtmpServer {
        self.resolve_block_header(directive, DirectiveContext::RtmpMain, "server");
        let children = directive.children.as_deref().unwrap_or_default();
        let policy =
            self.resolve_local_policy(children, DirectiveContext::RtmpServer, inherited.clone());
        let mut listens = Vec::new();
        let mut applications = Vec::new();
        let mut application_names = HashMap::new();
        let mut outbound_chunk_size = None;
        let mut chunk_size_origin = None;

        for child in children {
            match child.directive.name.value.as_slice() {
                b"listen" => listens.push(self.resolve_listen(child)),
                b"chunk_size" => {
                    if let Some(value) = self.resolve_server_chunk_size(child) {
                        outbound_chunk_size = Some(value);
                        chunk_size_origin = Some(Self::origin(child));
                    }
                }
                b"application" => {
                    let application = self.resolve_application(child, &policy);
                    if let Some(name) = &application.name {
                        if let Some(first) = application_names.get(&name.value).copied() {
                            self.block_related(
                                child.occurrence,
                                E_DUPLICATE_IDENTITY,
                                "duplicate nginx-RTMP application name in one server",
                                first,
                            );
                        } else {
                            application_names.insert(name.value.clone(), child.occurrence);
                        }
                    }
                    applications.push(application);
                }
                b"hls_muxdelay" => self.resolve_source_noop(child, DirectiveContext::RtmpServer),
                name if is_supported_policy(name) => {}
                _ => self.resolve_unsupported_subtree(
                    child,
                    DirectiveContext::RtmpServer,
                    unsupported_rtmp_reason(&child.directive.name.value),
                ),
            }
        }

        if listens.is_empty() {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "nginx-RTMP server requires an explicit socket listen",
            );
        }
        if applications.is_empty() {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "nginx-RTMP server requires at least one application",
            );
        }
        EffectiveRtmpServer {
            origin: Self::origin(directive),
            outbound_chunk_size,
            chunk_size_origin,
            listens,
            applications,
        }
    }

    fn resolve_server_chunk_size(&mut self, directive: &ExpandedDirective) -> Option<u32> {
        if self
            .validate_registered(directive, DirectiveContext::RtmpServer)
            .is_err()
            || directive.directive.children.is_some()
            || directive.directive.arguments.len() != 1
        {
            return None;
        }
        match parse_u32(&directive.directive.arguments[0].value) {
            Some(value) if (1..=MAX_OUTBOUND_CHUNK_SIZE).contains(&value) => {
                self.resolved(directive.occurrence);
                Some(value)
            }
            _ => {
                self.block(
                    directive.occurrence,
                    E_INVALID_VALUE,
                    "chunk_size is outside canonical RTMP outbound chunk bounds",
                );
                None
            }
        }
    }

    fn resolve_listen(&mut self, directive: &ExpandedDirective) -> EffectiveRtmpListen {
        let value = directive
            .directive
            .arguments
            .first()
            .map(|word| self.value(word));
        let registered = self.validate_registered(directive, DirectiveContext::RtmpServer);
        let address = value
            .as_ref()
            .and_then(|value| parse_rtmp_socket(&value.value));
        let outcome = if directive.directive.children.is_some() {
            Some((E_INVALID_VALUE, "listen must end with a semicolon"))
        } else if registered.is_err() {
            None
        } else if directive.directive.arguments.len() != 1 {
            Some((
                E_UNSUPPORTED_FEATURE,
                "nginx-RTMP listen options are not represented canonically",
            ))
        } else if address.is_none() {
            Some((
                E_INVALID_VALUE,
                "nginx-RTMP listen must be a numeric socket address with a nonzero port",
            ))
        } else {
            None
        };
        self.finish_occurrence(directive.occurrence, outcome);
        EffectiveRtmpListen {
            origin: Self::origin(directive),
            value,
            address,
        }
    }

    #[allow(clippy::too_many_lines)]
    fn resolve_application(
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
                {
                    if let Some(first) = recorder_names.insert(name.to_owned(), child.occurrence) {
                        self.block_related(
                            child.occurrence,
                            E_DUPLICATE_IDENTITY,
                            "duplicate nginx-RTMP recorder name in one application",
                            first,
                        );
                    }
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

    fn resolve_local_policy(
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
            if !repeatable {
                if let Some(first) = seen.insert(name.to_vec(), child.occurrence) {
                    self.block_related(
                        child.occurrence,
                        E_DUPLICATE_IDENTITY,
                        "duplicate nginx-RTMP scalar directive in one context",
                        first,
                    );
                    continue;
                }
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

    fn resolve_block_header(
        &mut self,
        directive: &ExpandedDirective,
        context: DirectiveContext,
        name: &'static str,
    ) {
        if self.validate_registered(directive, context).is_err() {
            return;
        }
        if directive.children.is_none() {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "nginx-RTMP structural directive requires a block",
            );
        } else {
            debug_assert_eq!(directive.directive.name.value, name.as_bytes());
            self.resolved(directive.occurrence);
        }
    }

    fn resolve_unsupported_subtree(
        &mut self,
        directive: &ExpandedDirective,
        context: DirectiveContext,
        message: &'static str,
    ) {
        if self.validate_registered(directive, context).is_ok() {
            self.block(directive.occurrence, E_UNSUPPORTED_FEATURE, message);
        }
        for child in directive.children.as_deref().unwrap_or_default() {
            self.block_subtree(child, message);
        }
    }

    fn resolve_source_noop(&mut self, directive: &ExpandedDirective, context: DirectiveContext) {
        if self.validate_registered(directive, context).is_ok() {
            self.resolved(directive.occurrence);
        }
    }

    fn validate_registered(
        &mut self,
        directive: &ExpandedDirective,
        context: DirectiveContext,
    ) -> Result<(), ()> {
        let Ok(name) = std::str::from_utf8(&directive.directive.name.value) else {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "nginx-RTMP directive name must be UTF-8",
            );
            return Err(());
        };
        let Some(arguments) = directive
            .directive
            .arguments
            .iter()
            .map(|argument| std::str::from_utf8(&argument.value).ok())
            .collect::<Option<Vec<_>>>()
        else {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "nginx-RTMP directive arguments must be UTF-8",
            );
            return Err(());
        };
        match validate_directive(name, context, &arguments) {
            Ok(_) => Ok(()),
            Err(DirectiveError::UnknownDirective(_) | DirectiveError::InvalidContext { .. }) => {
                self.block(
                    directive.occurrence,
                    E_UNSUPPORTED_FEATURE,
                    "directive is not registered for this nginx-RTMP context",
                );
                Err(())
            }
            Err(DirectiveError::InvalidArity { .. } | DirectiveError::InvalidValue { .. }) => {
                self.block(
                    directive.occurrence,
                    E_INVALID_VALUE,
                    "nginx-RTMP directive has invalid registered syntax",
                );
                Err(())
            }
        }
    }

    fn is_registered_in_context(directive: &ExpandedDirective, context: DirectiveContext) -> bool {
        let Ok(name) = std::str::from_utf8(&directive.directive.name.value) else {
            return false;
        };
        oxiroute_rtmp::directive_specs()
            .iter()
            .any(|spec| spec.name == name && spec.contexts.contains(&context))
    }

    fn reject_overlapping_listens(&mut self, blocks: &[EffectiveRtmp]) {
        let listens = blocks
            .iter()
            .flat_map(|block| &block.servers)
            .flat_map(|server| &server.listens)
            .filter_map(|listen| {
                listen
                    .address
                    .map(|address| (address, listen.origin.occurrence))
            })
            .collect::<Vec<_>>();
        for (index, (first_address, first_origin)) in listens.iter().enumerate() {
            for (second_address, second_origin) in &listens[index + 1..] {
                if sockets_overlap(*first_address, *second_address) {
                    self.block_related(
                        *second_origin,
                        E_DUPLICATE_IDENTITY,
                        "nginx-RTMP listen sockets overlap in canonical listener semantics",
                        *first_origin,
                    );
                }
            }
        }
    }

    fn structural_subtree(&mut self, directive: &ExpandedDirective) {
        self.structural(directive.occurrence);
        for child in directive.children.as_deref().unwrap_or_default() {
            self.structural_subtree(child);
        }
    }

    fn block_subtree(&mut self, directive: &ExpandedDirective, message: &'static str) {
        self.block(directive.occurrence, E_UNSUPPORTED_FEATURE, message);
        for child in directive.children.as_deref().unwrap_or_default() {
            self.block_subtree(child, message);
        }
    }

    fn classify_remaining(&mut self) {
        for index in 0..self.graph.expanded_occurrences.len() {
            if self.dispositions[index].is_some() {
                continue;
            }
            let occurrence = &self.graph.expanded_occurrences[index];
            if occurrence.directive.name.value == b"include" {
                if occurrence.directive.arguments.len() != 1
                    || occurrence.directive.children.is_some()
                {
                    self.block(
                        occurrence.id,
                        E_INVALID_VALUE,
                        "include requires one path and a semicolon",
                    );
                } else {
                    let failure = self
                        .graph
                        .includes
                        .iter()
                        .find(|edge| edge.occurrence == occurrence.id)
                        .and_then(include_failure);
                    self.dispositions[index] = Some(failure.map_or(
                        OccurrenceDisposition::Structural,
                        OccurrenceDisposition::Blocking,
                    ));
                }
            } else {
                self.block(
                    occurrence.id,
                    E_UNSUPPORTED_FEATURE,
                    "reachable nginx-RTMP directive was not resolved",
                );
            }
        }
    }

    fn finish_occurrence(
        &mut self,
        occurrence: OccurrenceId,
        outcome: Option<(DiagnosticCode, &'static str)>,
    ) {
        if let Some((code, message)) = outcome {
            self.block(occurrence, code, message);
        } else if self.dispositions[occurrence.get()].is_none() {
            self.resolved(occurrence);
        }
    }

    fn resolved(&mut self, occurrence: OccurrenceId) {
        if self.dispositions[occurrence.get()].is_none() {
            self.dispositions[occurrence.get()] = Some(OccurrenceDisposition::Resolved);
        }
    }

    fn structural(&mut self, occurrence: OccurrenceId) {
        if self.dispositions[occurrence.get()].is_none() {
            self.dispositions[occurrence.get()] = Some(OccurrenceDisposition::Structural);
        }
    }

    fn block(&mut self, occurrence: OccurrenceId, code: DiagnosticCode, message: &'static str) {
        if matches!(
            self.dispositions[occurrence.get()],
            Some(OccurrenceDisposition::Blocking(_))
        ) {
            return;
        }
        self.dispositions[occurrence.get()] = Some(OccurrenceDisposition::Blocking(code));
        let expanded = self.occurrence(occurrence);
        self.diagnostics.push(
            Diagnostic::new(code, Severity::Error, DiagnosticStage::Resolve, message)
                .with_primary_span(expanded.directive.span)
                .with_include_stack(
                    expanded
                        .provenance
                        .include_stack
                        .iter()
                        .map(|frame| frame.directive_span),
                ),
        );
    }

    fn block_related(
        &mut self,
        occurrence: OccurrenceId,
        code: DiagnosticCode,
        message: &'static str,
        first: OccurrenceId,
    ) {
        if matches!(
            self.dispositions[occurrence.get()],
            Some(OccurrenceDisposition::Blocking(_))
        ) {
            return;
        }
        self.dispositions[occurrence.get()] = Some(OccurrenceDisposition::Blocking(code));
        let expanded = self.occurrence(occurrence);
        let first = self.occurrence(first);
        self.diagnostics.push(
            Diagnostic::new(code, Severity::Error, DiagnosticStage::Resolve, message)
                .with_primary_span(expanded.directive.span)
                .with_include_stack(
                    expanded
                        .provenance
                        .include_stack
                        .iter()
                        .map(|frame| frame.directive_span),
                )
                .with_related_span(first.directive.span, "first identity declared here"),
        );
    }

    fn occurrence(&self, id: OccurrenceId) -> &ExpandedOccurrence {
        let occurrence = &self.graph.expanded_occurrences[id.get()];
        debug_assert_eq!(occurrence.id, id);
        occurrence
    }

    fn origin(directive: &ExpandedDirective) -> DirectiveOrigin {
        DirectiveOrigin {
            occurrence: directive.occurrence,
            span: directive.directive.span,
            provenance: directive.provenance.clone(),
        }
    }

    fn value(&self, word: &Word) -> NginxValue {
        let source = self
            .graph
            .source(word.span.source())
            .expect("expanded word source is retained in the graph");
        NginxValue {
            value: word.value.clone(),
            raw: source
                .source
                .slice(word.span.range())
                .expect("expanded word span is within its source")
                .to_vec(),
            span: word.span,
        }
    }

    fn decision(&self, occurrence: &ExpandedOccurrence) -> OccurrenceDecision {
        OccurrenceDecision {
            occurrence: occurrence.id,
            parent: occurrence.parent,
            name: self.value(&occurrence.directive.name),
            arguments: occurrence
                .directive
                .arguments
                .iter()
                .map(|word| self.value(word))
                .collect(),
            span: occurrence.directive.span,
            provenance: occurrence.provenance.clone(),
            disposition: self.dispositions[occurrence.id.get()]
                .expect("every expanded occurrence has a terminal disposition"),
        }
    }
}

fn is_supported_policy(name: &[u8]) -> bool {
    matches!(
        name,
        b"allow"
            | b"deny"
            | b"max_connections"
            | b"live"
            | b"idle_streams"
            | b"exec"
            | b"exec_push"
            | b"exec_publish"
            | b"exec_publish_done"
            | b"respawn"
            | b"respawn_timeout"
            | b"record"
            | b"record_path"
            | b"record_suffix"
            | b"record_unique"
            | b"record_interval"
            | b"record_append"
            | b"record_lock"
            | b"record_max_size"
            | b"record_max_frames"
            | b"record_notify"
            | b"rtmp_auto_push"
            | b"rtmp_auto_push_reconnect"
            | b"rtmp_socket_dir"
            | b"hls"
            | b"hls_fragment"
            | b"hls_max_fragment"
            | b"hls_path"
            | b"hls_playlist_length"
            | b"hls_nested"
            | b"hls_fragment_naming"
            | b"hls_cleanup"
            | b"hls_keys"
            | b"hls_key_url"
            | b"hls_fragments_per_key"
    )
}

fn is_root_auto_push_policy(name: &[u8]) -> bool {
    matches!(
        name,
        b"rtmp_auto_push" | b"rtmp_auto_push_reconnect" | b"rtmp_socket_dir"
    )
}

fn unsupported_rtmp_reason(name: &[u8]) -> &'static str {
    match name {
        b"allow" | b"deny" => {
            "nginx-RTMP publish/play access rules have no canonical RTMP authorization policy"
        }
        b"push"
        | b"pull"
        | b"relay_buffer"
        | b"push_reconnect"
        | b"pull_reconnect"
        | b"session_relay"
        | b"rtmp_auto_push"
        | b"rtmp_auto_push_reconnect"
        | b"rtmp_socket_dir" => {
            "nginx-RTMP relay and auto-push topology has no canonical RTMP relay model"
        }
        b"exec" | b"exec_push" | b"exec_pull" | b"exec_publish" | b"exec_publish_done"
        | b"exec_play" | b"exec_play_done" | b"exec_record_done" | b"exec_static" | b"respawn"
        | b"respawn_timeout" | b"exec_kill_signal" | b"exec_options" => {
            "nginx-RTMP process hooks have no canonical command-execution model"
        }
        b"play" | b"play_temp_path" | b"play_local_path" | b"netcall_timeout"
        | b"netcall_buffer" => {
            "nginx-RTMP VOD and netcall sources have no canonical playback-source model"
        }
        b"on_connect"
        | b"on_disconnect"
        | b"on_publish"
        | b"on_play"
        | b"on_publish_done"
        | b"on_play_done"
        | b"on_done"
        | b"on_record_done"
        | b"on_update"
        | b"notify_method"
        | b"notify_update_timeout"
        | b"notify_update_strict"
        | b"notify_relay_redirect" => {
            "nginx-RTMP HTTP callbacks have no canonical notification policy"
        }
        b"access_log" | b"log_format" => {
            "only one absolute rtmp-scope access_log with the combined format has canonical semantics"
        }
        b"max_connections" => {
            "nginx-RTMP max_connections is one process-wide RTMP CONNECT cap, not a per-listener connection cap"
        }
        name if name.starts_with(b"hls_") || name == b"hls" => {
            "nginx-RTMP HLS packaging has no canonical media-packaging service"
        }
        name if name.starts_with(b"dash_") || name == b"dash" => {
            "nginx-RTMP DASH packaging has no canonical media-packaging service"
        }
        b"so_keepalive" | b"timeout" | b"ping" | b"ping_timeout" | b"max_streams"
        | b"ack_window" | b"chunk_size" | b"max_message" | b"out_queue" | b"out_cork" | b"busy"
        | b"play_time_fix" | b"publish_time_fix" | b"buflen" => {
            "nginx-RTMP transport and session tuning has no canonical RTMP session policy"
        }
        b"meta"
        | b"stream_buckets"
        | b"buffer"
        | b"sync"
        | b"interleave"
        | b"wait_key"
        | b"wait_video"
        | b"publish_notify"
        | b"play_restart"
        | b"drop_idle_publisher" => {
            "nginx-RTMP stream timing and codec behavior has no canonical application policy"
        }
        _ => "registered nginx-RTMP behavior has no canonical or runtime abstraction",
    }
}

fn valid_exec_path(value: &str) -> bool {
    value.starts_with('/')
        && !value.is_empty()
        && !value.ends_with('/')
        && value.len() <= 4_096
        && !value
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
        && value.strip_prefix('/').is_some_and(|value| {
            value
                .split('/')
                .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
        })
}

fn parse_access_rule(
    directive: &ExpandedDirective,
    name: &[u8],
    origin: DirectiveOrigin,
) -> Option<EffectiveRtmpAccessRule> {
    let (operation, target) = match directive.directive.arguments.as_slice() {
        [target] => (AccessOperation::Both, target),
        [operation, target] => (
            match operation.value.as_slice() {
                b"publish" => AccessOperation::Publish,
                b"play" => AccessOperation::Play,
                _ => return None,
            },
            target,
        ),
        _ => return None,
    };
    let network = std::str::from_utf8(&target.value).ok()?.to_owned();
    Some(EffectiveRtmpAccessRule {
        action: match name {
            b"allow" => RtmpAclAction::Allow,
            b"deny" => RtmpAclAction::Deny,
            _ => return None,
        },
        network,
        origin,
        operation,
    })
}

fn parse_record(arguments: &[Word]) -> Result<(RecordSetting, RtmpRecordMask), &'static str> {
    let values = arguments
        .iter()
        .map(|argument| argument.value.as_slice())
        .collect::<Vec<_>>();
    let mut audio = false;
    let mut video = false;
    let mut keyframes = false;
    let mut manual = false;
    let mut off = false;
    for value in values {
        match value {
            b"off" => off = true,
            b"all" => {
                audio = true;
                video = true;
            }
            b"audio" => audio = true,
            b"video" => video = true,
            b"keyframes" => {
                video = true;
                keyframes = true;
            }
            b"manual" => manual = true,
            _ => return Err("nginx-RTMP record bitmask is not exactly representable"),
        }
    }
    if off {
        return Ok((RecordSetting::Off, RtmpRecordMask::default()));
    }
    if !audio && !video {
        return Err(
            "bare record manual has no nginx audio/video bits and is not canonical recording",
        );
    }
    Ok((
        if manual {
            RecordSetting::Manual
        } else {
            RecordSetting::Continuous
        },
        RtmpRecordMask {
            audio,
            video,
            keyframes,
        },
    ))
}

fn parse_u32(value: &[u8]) -> Option<u32> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

fn parse_u64(value: &[u8]) -> Option<u64> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

fn valid_canonical_name(value: &[u8]) -> bool {
    std::str::from_utf8(value).is_ok_and(|name| {
        !name.trim().is_empty() && name.trim() == name && !name.chars().any(char::is_control)
    })
}

fn secure_recording_root(value: &[u8]) -> Option<PathBuf> {
    let value = std::str::from_utf8(value).ok()?;
    if value.len() > MAX_RECORDING_ROOT_BYTES
        || !value.starts_with('/')
        || value == "/"
        || value.ends_with('/')
        || value.as_bytes().contains(&0)
        || value
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
    {
        return None;
    }
    Some(PathBuf::from(value))
}

fn secure_auto_push_root(value: &[u8]) -> Option<PathBuf> {
    let value = std::str::from_utf8(value).ok()?;
    if value.len() > MAX_RECORDING_ROOT_BYTES
        || !value.starts_with('/')
        || value == "/"
        || value.ends_with('/')
        || value.as_bytes().contains(&0)
        || value
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
    {
        return None;
    }
    Some(PathBuf::from(value))
}

fn exact_suffix(value: &[u8]) -> Option<String> {
    let value = std::str::from_utf8(value).ok()?;
    if value.len() > MAX_SUFFIX_BYTES
        || value.as_bytes().contains(&0)
        || value
            .as_bytes()
            .iter()
            .any(|byte| matches!(*byte, b'/' | b'\\'))
    {
        return None;
    }
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if !matches!(
            bytes.get(index + 1),
            Some(b'Y' | b'm' | b'd' | b'H' | b'M' | b'S' | b'%')
        ) {
            return None;
        }
        index += 2;
    }
    Some(value.to_owned())
}

fn parse_push_target(value: &[u8]) -> Option<(String, u16, String)> {
    let value = std::str::from_utf8(value).ok()?.strip_prefix("rtmp://")?;
    let (authority, application) = value.split_once('/')?;
    if authority.is_empty()
        || application.is_empty()
        || application.contains(['/', '?', '#'])
        || application.contains('$') && application != "$name"
    {
        return None;
    }
    let (host, port) = if let Some((host, port)) = authority.rsplit_once(':') {
        if host.is_empty() || host.contains(':') {
            return None;
        }
        (host, port.parse().ok()?)
    } else {
        (authority, 1_935)
    };
    if port == 0
        || host.contains(['@', '/', '?', '#'])
        || host.parse::<std::net::IpAddr>().is_err()
            && (!host.is_ascii() || host.split('.').any(str::is_empty))
    {
        return None;
    }
    Some((host.to_ascii_lowercase(), port, application.to_owned()))
}

fn parse_nginx_milliseconds(value: &[u8]) -> Option<u64> {
    let mut index = 0;
    let mut previous_unit = 2_u8;
    let mut total = 0_u64;
    while index < value.len() {
        let start = index;
        while value.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if start == index {
            return None;
        }
        let number = std::str::from_utf8(&value[start..index])
            .ok()?
            .parse::<u64>()
            .ok()?;
        let (unit, multiplier) = if index == value.len() {
            (9, 1)
        } else if value[index..].starts_with(b"ms") {
            index += 2;
            (8, 1)
        } else {
            let result = match value[index] {
                b'y' => (1, 365_u64 * 24 * 60 * 60 * 1_000),
                b'M' => (2, 30_u64 * 24 * 60 * 60 * 1_000),
                b'w' => (3, 7_u64 * 24 * 60 * 60 * 1_000),
                b'd' => (4, 24_u64 * 60 * 60 * 1_000),
                b'h' => (5, 60_u64 * 60 * 1_000),
                b'm' => (6, 60_u64 * 1_000),
                b's' => (7, 1_000_u64),
                _ => return None,
            };
            index += 1;
            result
        };
        if unit <= previous_unit {
            return None;
        }
        previous_unit = unit;
        total = total.checked_add(number.checked_mul(multiplier)?)?;
    }
    Some(total)
}

fn parse_nginx_size(value: &[u8]) -> Option<u64> {
    let (digits, multiplier) = match value.last().copied() {
        Some(b'k' | b'K') => (&value[..value.len() - 1], 1024_u64),
        Some(b'm' | b'M') => (&value[..value.len() - 1], 1024_u64.pow(2)),
        Some(b'g' | b'G') => (&value[..value.len() - 1], 1024_u64.pow(3)),
        Some(_) => (value, 1),
        None => return None,
    };
    std::str::from_utf8(digits)
        .ok()?
        .parse::<u64>()
        .ok()?
        .checked_mul(multiplier)
}

fn parse_rtmp_socket(value: &[u8]) -> Option<SocketAddr> {
    if value.iter().all(u8::is_ascii_digit) {
        let port = parse_port(value)?;
        return Some(SocketAddr::from(([0, 0, 0, 0], port)));
    }
    let text = std::str::from_utf8(value).ok()?;
    if let Some(port) = text
        .strip_prefix("*:")
        .and_then(|port| parse_port(port.as_bytes()))
    {
        return Some(SocketAddr::from(([0, 0, 0, 0], port)));
    }
    text.parse::<SocketAddr>()
        .ok()
        .filter(|address| address.port() != 0)
}

fn parse_port(value: &[u8]) -> Option<u16> {
    let port = std::str::from_utf8(value).ok()?.parse::<u16>().ok()?;
    (port != 0).then_some(port)
}

fn sockets_overlap(first: SocketAddr, second: SocketAddr) -> bool {
    if first.port() != second.port() || first.is_ipv4() != second.is_ipv4() {
        return false;
    }
    first.ip() == second.ip() || first.ip().is_unspecified() || second.ip().is_unspecified()
}

fn include_failure(edge: &super::IncludeEdge) -> Option<DiagnosticCode> {
    edge.failure.or_else(|| {
        edge.candidates
            .iter()
            .find_map(|candidate| match candidate.status {
                IncludeCandidateStatus::Expanded(_) => None,
                IncludeCandidateStatus::Cycle(_) => Some(E_INCLUDE_CYCLE),
                IncludeCandidateStatus::ExpansionLimit(_)
                | IncludeCandidateStatus::SourceSizeLimit
                | IncludeCandidateStatus::SourceFileLimit
                | IncludeCandidateStatus::AggregateSourceLimit => Some(E_SOURCE_LIMIT),
                IncludeCandidateStatus::CanonicalizeFailed
                | IncludeCandidateStatus::SourceChanged => Some(E_SOURCE_CHANGED),
                IncludeCandidateStatus::SourceIo => Some(E_SOURCE_IO),
            })
    })
}
