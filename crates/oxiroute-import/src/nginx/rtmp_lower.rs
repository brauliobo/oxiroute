use std::{
    collections::BTreeSet,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use oxiroute_config::{
    AccessLogPolicy, Config, DownstreamTimeoutPolicy, Listener, ListenerBind, Protocol,
    RtmpApplication, RtmpFanoutPolicy, RtmpPushTarget, RtmpRecorder, RtmpRecorderSegmentNaming,
    RtmpRecorderStart, RtmpRecorderTimeBasis, RtmpRecorderTimezone, RtmpService, validate_config,
};

use crate::{
    CanonicalDraft, CanonicalProvenance, Diagnostic, DiagnosticCode, DiagnosticStage,
    E_INVALID_VALUE, E_SEMANTICS_NOT_REPRESENTABLE, Report, Severity,
};

use super::{
    DirectiveOrigin, EffectiveRtmpApplication, EffectiveRtmpRecorder, EffectiveRtmpServer,
    OccurrenceDecision, OccurrenceDisposition, OccurrenceId, RtmpRecordMode, RtmpResolution,
    SourceGraph, load,
};

const DEFAULT_MAX_QUEUE_MESSAGES: u64 = 256;
const DEFAULT_MAX_QUEUE_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_MAX_ACTIVE_RECORDERS: u64 = 32;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockedRtmpService {
    pub path: String,
    pub server: OccurrenceId,
    pub binds: Vec<SocketAddr>,
    pub diagnostic_codes: Vec<DiagnosticCode>,
}

/// Complete nginx-RTMP import evidence and the optional validated canonical result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtmpImportReport {
    pub source_graph: SourceGraph,
    pub occurrence_ledger: Vec<OccurrenceDecision>,
    pub diagnostics: Vec<Diagnostic>,
    pub provenance: Vec<CanonicalProvenance<DirectiveOrigin>>,
    pub blocked_services: Vec<BlockedRtmpService>,
    pub draft: CanonicalDraft,
    pub config: Option<Config>,
    pub(crate) used_recording_root_overlay: bool,
}

impl RtmpImportReport {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
    }
}

/// Loads, resolves, lowers, and conditionally finalizes one nginx-RTMP source graph.
#[must_use]
pub fn import_rtmp(root: &Path, root_prefix: &Path) -> RtmpImportReport {
    lower_rtmp(load(root, root_prefix))
}

/// Imports nginx-RTMP with the host timezone that governed native local-time suffix rendering.
#[must_use]
pub fn import_rtmp_with_timezone(
    root: &Path,
    root_prefix: &Path,
    host_timezone: &str,
) -> RtmpImportReport {
    lower_rtmp_with_mode(load(root, root_prefix), false, Some(host_timezone), None)
}

pub(super) fn lower_rtmp(loaded: Report<SourceGraph>) -> RtmpImportReport {
    lower_rtmp_with_mode(loaded, false, None, None)
}

pub(super) fn lower_rtmp_root(
    loaded: Report<SourceGraph>,
    host_timezone: Option<&str>,
    recording_root: Option<&Path>,
) -> RtmpImportReport {
    lower_rtmp_with_mode(loaded, true, host_timezone, recording_root)
}

fn lower_rtmp_with_mode(
    loaded: Report<SourceGraph>,
    complete_root: bool,
    host_timezone: Option<&str>,
    recording_root: Option<&Path>,
) -> RtmpImportReport {
    let (graph, mut diagnostics) = loaded.into_parts();
    let resolved = if complete_root {
        super::rtmp_semantic::resolve_rtmp_root_graph(&graph)
    } else {
        super::rtmp_semantic::resolve_rtmp_graph(&graph)
    };
    let (resolution, resolve_diagnostics) = resolved.into_parts();
    diagnostics.extend(resolve_diagnostics);
    Lowerer::new(
        graph,
        resolution,
        diagnostics,
        host_timezone.map(str::to_owned),
        recording_root.map(Path::to_path_buf),
    )
    .run()
}

struct Lowerer {
    graph: SourceGraph,
    resolution: RtmpResolution,
    diagnostics: Vec<Diagnostic>,
    provenance: Vec<CanonicalProvenance<DirectiveOrigin>>,
    blocked_services: Vec<BlockedRtmpService>,
    draft: CanonicalDraft,
    host_timezone: Option<String>,
    recording_root: Option<PathBuf>,
    used_recording_root_overlay: bool,
}

impl Lowerer {
    fn new(
        graph: SourceGraph,
        resolution: RtmpResolution,
        diagnostics: Vec<Diagnostic>,
        host_timezone: Option<String>,
        recording_root: Option<PathBuf>,
    ) -> Self {
        Self {
            graph,
            resolution,
            diagnostics,
            provenance: Vec::new(),
            blocked_services: Vec::new(),
            draft: CanonicalDraft::default(),
            host_timezone,
            recording_root,
            used_recording_root_overlay: false,
        }
    }

    fn run(mut self) -> RtmpImportReport {
        let blocks = self.resolution.rtmp_blocks.clone();
        if self.recording_root.is_some() {
            let native_roots = blocks
                .iter()
                .flat_map(|rtmp| &rtmp.servers)
                .flat_map(|server| &server.applications)
                .flat_map(|application| &application.policy.recorders)
                .map(|recorder| recorder.root_directory.clone())
                .collect::<BTreeSet<_>>();
            if native_roots.len() != 1 {
                self.recording_root = None;
            }
        }
        for (rtmp_index, rtmp) in blocks.iter().enumerate() {
            for (server_index, server) in rtmp.servers.iter().enumerate() {
                let path = format!("/nginx/rtmp/{rtmp_index}/servers/{server_index}");
                let mut codes =
                    self.blocking_codes(server.origin.occurrence, rtmp.origin.occurrence);
                if self.host_timezone.is_none()
                    && server
                        .applications
                        .iter()
                        .any(|application| !application.policy.recorders.is_empty())
                {
                    codes.push(E_SEMANTICS_NOT_REPRESENTABLE);
                    self.diagnostics.push(
                        Diagnostic::new(
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            Severity::Error,
                            DiagnosticStage::Lower,
                            "nginx recording suffixes require an explicit host IANA timezone overlay",
                        )
                        .with_primary_span(server.origin.span),
                    );
                }
                if codes.is_empty() {
                    self.lower_server(server, rtmp, rtmp_index, server_index);
                } else {
                    self.blocked_services.push(BlockedRtmpService {
                        path,
                        server: server.origin.occurrence,
                        binds: server
                            .listens
                            .iter()
                            .filter_map(|listen| listen.address)
                            .collect(),
                        diagnostic_codes: codes,
                    });
                }
            }
        }

        let mut config = self.draft.to_config();
        let finalizable = self.blocked_services.is_empty()
            && !self
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity() == Severity::Error);
        let config = if finalizable {
            match validate_config(&mut config) {
                Ok(()) => Some(config),
                Err(error) => {
                    self.diagnostics.push(Diagnostic::new(
                        E_INVALID_VALUE,
                        Severity::Error,
                        DiagnosticStage::Validate,
                        format!("lowered canonical RTMP configuration is invalid: {error}"),
                    ));
                    None
                }
            }
        } else {
            None
        };
        let ((), diagnostics) = Report::new((), self.diagnostics).into_parts();

        RtmpImportReport {
            source_graph: self.graph,
            occurrence_ledger: self.resolution.decisions,
            diagnostics,
            provenance: self.provenance,
            blocked_services: self.blocked_services,
            draft: self.draft,
            config,
            used_recording_root_overlay: self.used_recording_root_overlay,
        }
    }

    fn lower_server(
        &mut self,
        server: &EffectiveRtmpServer,
        rtmp: &super::EffectiveRtmp,
        rtmp_index: usize,
        server_index: usize,
    ) {
        let service_index = self.draft.rtmp_services.len();
        let service_name = format!("nginx-rtmp-service-{rtmp_index}-{server_index}");
        let applications = server
            .applications
            .iter()
            .enumerate()
            .map(|(application_index, application)| {
                self.lower_application(application, service_index, application_index)
            })
            .collect();

        self.draft.rtmp_services.push(RtmpService {
            name: service_name.clone(),
            outbound_chunk_size: rtmp.outbound_chunk_size,
            access_log: rtmp
                .access_log_disabled
                .then_some(AccessLogPolicy::Disabled),
            applications,
        });
        self.provenance.push(CanonicalProvenance {
            path: format!("/rtmp_services/{service_index}"),
            origins: std::iter::once(server.origin.clone())
                .chain(rtmp.chunk_size_origin.clone())
                .chain(rtmp.access_log_origin.clone())
                .collect(),
        });
        if let Some(origin) = &rtmp.chunk_size_origin {
            self.provenance.push(CanonicalProvenance {
                path: format!("/rtmp_services/{service_index}/outbound_chunk_size"),
                origins: vec![origin.clone()],
            });
        }
        if let Some(origin) = &rtmp.access_log_origin {
            self.provenance.push(CanonicalProvenance {
                path: format!("/rtmp_services/{service_index}/access_log"),
                origins: vec![origin.clone()],
            });
        }

        for listen in &server.listens {
            let address = listen
                .address
                .expect("unblocked nginx-RTMP listen has a socket address");
            let listener_index = self.draft.listeners.len();
            self.draft.listeners.push(Listener {
                name: format!("nginx-rtmp-listener-{rtmp_index}-{server_index}-{listener_index}"),
                bind: ListenerBind::Socket { address },
                protocol: Protocol::Rtmp,
                service: Some(service_name.clone()),
                tls_profile: None,
                max_connections: None,
                downstream_timeouts: DownstreamTimeoutPolicy::default(),
            });
            self.provenance.push(CanonicalProvenance {
                path: format!("/listeners/{listener_index}"),
                origins: vec![server.origin.clone(), listen.origin.clone()],
            });
        }
    }

    fn lower_application(
        &mut self,
        application: &EffectiveRtmpApplication,
        service_index: usize,
        application_index: usize,
    ) -> RtmpApplication {
        let name = std::str::from_utf8(
            &application
                .name
                .as_ref()
                .expect("unblocked application has one name")
                .value,
        )
        .expect("unblocked application name is UTF-8")
        .to_owned();
        let application_path =
            format!("/rtmp_services/{service_index}/applications/{application_index}");
        self.provenance.push(CanonicalProvenance {
            path: application_path.clone(),
            origins: vec![application.origin.clone()],
        });
        self.provenance.push(CanonicalProvenance {
            path: format!("{application_path}/live"),
            origins: vec![
                application
                    .policy
                    .live_origin
                    .clone()
                    .unwrap_or_else(|| application.origin.clone()),
            ],
        });
        self.provenance.push(CanonicalProvenance {
            path: format!("{application_path}/idle_streams"),
            origins: vec![
                application
                    .policy
                    .idle_streams_origin
                    .clone()
                    .unwrap_or_else(|| application.origin.clone()),
            ],
        });
        for (target_index, target) in application.push_targets.iter().enumerate() {
            self.provenance.push(CanonicalProvenance {
                path: format!("{application_path}/push_targets/{target_index}"),
                origins: vec![target.origin.clone()],
            });
        }

        let recorders = application
            .policy
            .recorders
            .iter()
            .enumerate()
            .map(|(recorder_index, recorder)| {
                self.lower_recorder(recorder, &application_path, recorder_index)
            })
            .collect();
        RtmpApplication {
            name,
            live: application.policy.live,
            idle_streams: application.policy.idle_streams,
            push_targets: application
                .push_targets
                .iter()
                .map(|target| RtmpPushTarget {
                    host: target.host.clone(),
                    port: target.port,
                    application: target.application.clone(),
                })
                .collect(),
            fanout: RtmpFanoutPolicy {
                max_subscribers: 1_024,
                max_queue_messages_per_subscriber: 256,
                max_queue_bytes_per_subscriber: 8 * 1024 * 1024,
            },
            recorders,
        }
    }

    fn lower_recorder(
        &mut self,
        recorder: &EffectiveRtmpRecorder,
        application_path: &str,
        recorder_index: usize,
    ) -> RtmpRecorder {
        let path = format!("{application_path}/recorders/{recorder_index}");
        self.provenance.push(CanonicalProvenance {
            path: path.clone(),
            origins: vec![recorder.record_origin.clone()],
        });
        self.provenance.push(CanonicalProvenance {
            path: format!("{path}/name"),
            origins: vec![recorder.name_origin.clone()],
        });
        for (field, origin) in [
            ("root_directory", Some(recorder.path_origin.clone())),
            ("suffix_template", recorder.suffix_origin.clone()),
            ("append_unix_seconds", recorder.unique_origin.clone()),
            ("rotation_interval_ms", recorder.interval_origin.clone()),
        ] {
            self.provenance.push(CanonicalProvenance {
                path: format!("{path}/{field}"),
                origins: vec![origin.unwrap_or_else(|| recorder.record_origin.clone())],
            });
        }

        RtmpRecorder {
            name: recorder.name.clone(),
            start: match recorder.mode {
                RtmpRecordMode::Continuous => RtmpRecorderStart::Continuous,
                RtmpRecordMode::Manual => RtmpRecorderStart::Manual,
            },
            root_directory: self.recording_root.as_ref().map_or_else(
                || recorder.root_directory.clone(),
                |root| {
                    self.used_recording_root_overlay = true;
                    root.clone()
                },
            ),
            suffix_template: recorder.suffix_template.clone(),
            append_unix_seconds: recorder.append_unix_seconds,
            timezone: RtmpRecorderTimezone::Iana(
                self.host_timezone
                    .clone()
                    .expect("recording services require an explicit timezone overlay"),
            ),
            time_basis: RtmpRecorderTimeBasis::SegmentStart,
            segment_naming: RtmpRecorderSegmentNaming::NginxCompatible,
            rotation_interval_ms: recorder.rotation_interval_ms,
            max_queue_messages: DEFAULT_MAX_QUEUE_MESSAGES,
            max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
            shutdown_timeout_ms: DEFAULT_SHUTDOWN_TIMEOUT_MS,
            max_storage_bytes: None,
            max_storage_files: None,
            max_active_recorders: DEFAULT_MAX_ACTIVE_RECORDERS,
        }
    }

    fn blocking_codes(&self, server: OccurrenceId, rtmp: OccurrenceId) -> Vec<DiagnosticCode> {
        let mut codes = Vec::new();
        for decision in &self.resolution.decisions {
            let OccurrenceDisposition::Blocking(code) = decision.disposition else {
                continue;
            };
            let affects_server = self.is_descendant(decision.occurrence, server)
                || (self.is_descendant(server, decision.occurrence)
                    && self.is_descendant(decision.occurrence, rtmp))
                || self.is_rtmp_global(decision.occurrence, rtmp)
                || Self::is_root_rtmp_policy(decision);
            if affects_server && !codes.contains(&code) {
                codes.push(code);
            }
        }
        codes
    }

    fn is_rtmp_global(&self, occurrence: OccurrenceId, rtmp: OccurrenceId) -> bool {
        let mut current = occurrence;
        loop {
            if current == rtmp {
                return true;
            }
            let Some(item) = self.graph.expanded_occurrences.get(current.get()) else {
                return false;
            };
            if item.directive.name.value == b"server" && item.parent == Some(rtmp) {
                return false;
            }
            let Some(parent) = item.parent else {
                return false;
            };
            current = parent;
        }
    }

    fn is_root_rtmp_policy(decision: &OccurrenceDecision) -> bool {
        if decision.parent.is_some() {
            return false;
        }
        let Ok(name) = std::str::from_utf8(&decision.name.value) else {
            return false;
        };
        oxiroute_rtmp::directive_specs().iter().any(|spec| {
            spec.name == name
                && spec
                    .contexts
                    .contains(&oxiroute_rtmp::DirectiveContext::NginxMain)
                && spec.name != "rtmp"
        })
    }

    fn is_descendant(&self, occurrence: OccurrenceId, ancestor: OccurrenceId) -> bool {
        let mut current = Some(occurrence);
        while let Some(id) = current {
            if id == ancestor {
                return true;
            }
            current = self
                .graph
                .expanded_occurrences
                .get(id.get())
                .and_then(|item| item.parent);
        }
        false
    }
}
