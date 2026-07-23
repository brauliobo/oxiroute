use std::{net::SocketAddr, path::Path};

use oxiroute_config::{
    Config, Listener, ListenerBind, Protocol, RtmpApplication, RtmpRecorder, RtmpRecorderStart,
    RtmpService, validate_config,
};

use crate::{
    CanonicalDraft, CanonicalProvenance, Diagnostic, DiagnosticCode, DiagnosticStage,
    E_INVALID_VALUE, Report, Severity,
};

use super::{
    DirectiveOrigin, EffectiveRtmpApplication, EffectiveRtmpRecorder, EffectiveRtmpServer,
    OccurrenceDecision, OccurrenceDisposition, OccurrenceId, RtmpRecordMode, RtmpResolution,
    SourceGraph, load,
};

const DEFAULT_MAX_QUEUE_MESSAGES: u64 = 256;
const DEFAULT_MAX_QUEUE_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_MAX_STORAGE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const DEFAULT_MAX_STORAGE_FILES: u64 = 10_000;
const DEFAULT_MAX_ACTIVE_RECORDERS: u64 = 8;

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
    let (graph, mut diagnostics) = load(root, root_prefix).into_parts();
    let (resolution, resolve_diagnostics) =
        super::rtmp_semantic::resolve_rtmp_graph(&graph).into_parts();
    diagnostics.extend(resolve_diagnostics);
    Lowerer::new(graph, resolution, diagnostics).run()
}

struct Lowerer {
    graph: SourceGraph,
    resolution: RtmpResolution,
    diagnostics: Vec<Diagnostic>,
    provenance: Vec<CanonicalProvenance<DirectiveOrigin>>,
    blocked_services: Vec<BlockedRtmpService>,
    draft: CanonicalDraft,
}

impl Lowerer {
    fn new(graph: SourceGraph, resolution: RtmpResolution, diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            graph,
            resolution,
            diagnostics,
            provenance: Vec::new(),
            blocked_services: Vec::new(),
            draft: CanonicalDraft::default(),
        }
    }

    fn run(mut self) -> RtmpImportReport {
        let blocks = self.resolution.rtmp_blocks.clone();
        for (rtmp_index, rtmp) in blocks.iter().enumerate() {
            for (server_index, server) in rtmp.servers.iter().enumerate() {
                let path = format!("/nginx/rtmp/{rtmp_index}/servers/{server_index}");
                let codes = self.blocking_codes(server.origin.occurrence, rtmp.origin.occurrence);
                if codes.is_empty() {
                    self.lower_server(server, rtmp_index, server_index);
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
        }
    }

    fn lower_server(
        &mut self,
        server: &EffectiveRtmpServer,
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
            applications,
        });
        self.provenance.push(CanonicalProvenance {
            path: format!("/rtmp_services/{service_index}"),
            origins: vec![server.origin.clone()],
        });

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
            root_directory: recorder.root_directory.clone(),
            suffix_template: recorder.suffix_template.clone(),
            append_unix_seconds: recorder.append_unix_seconds,
            rotation_interval_ms: recorder.rotation_interval_ms,
            max_queue_messages: DEFAULT_MAX_QUEUE_MESSAGES,
            max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
            shutdown_timeout_ms: DEFAULT_SHUTDOWN_TIMEOUT_MS,
            max_storage_bytes: DEFAULT_MAX_STORAGE_BYTES,
            max_storage_files: DEFAULT_MAX_STORAGE_FILES,
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
