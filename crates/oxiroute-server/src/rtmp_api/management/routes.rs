#[derive(Clone, Copy)]
pub(in super::super) enum Route<'a> {
    Listeners,
    ListenerState,
    Pools,
    PoolState,
    Servers,
    ServerState,
    ServerHealth,
    ServerChecks,
    ServerMaxConnections,
    ServerRefreshDns,
    Generations,
    GenerationReload,
    GenerationRollback,
    GenerationDrain,
    Tls,
    TlsReconcile,
    TlsRenew,
    TlsRevoke,
    TlsDelete,
    TlsAccountRollover,
    TlsJobCancel,
    TlsJobPause,
    TlsJobResume,
    Audit(Option<&'a str>),
    AuditStatus,
    Events(EventApiVersion, Option<&'a str>),
    EventStream(EventApiVersion, Option<&'a str>),
    ProcessDrain,
    ProcessShutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in super::super) enum EventApiVersion {
    V1,
    V2,
}

pub(in super::super) fn match_route(path_and_query: &str) -> Option<Route<'_>> {
    let (path, query) = path_and_query
        .split_once('?')
        .map_or((path_and_query, None), |(path, query)| (path, Some(query)));
    match path {
        "/api/v1/listeners/administrative-state" => Some(Route::ListenerState),
        "/api/v1/pools/administrative-state" => Some(Route::PoolState),
        "/api/v1/servers/administrative-state" => Some(Route::ServerState),
        "/api/v1/servers/health-override" => Some(Route::ServerHealth),
        "/api/v1/servers/checks" => Some(Route::ServerChecks),
        "/api/v1/servers/max-connections" => Some(Route::ServerMaxConnections),
        "/api/v1/servers/refresh-dns" => Some(Route::ServerRefreshDns),
        "/api/v1/generations/reload" => Some(Route::GenerationReload),
        "/api/v1/generations/rollback" => Some(Route::GenerationRollback),
        "/api/v1/generations/drain" => Some(Route::GenerationDrain),
        "/api/v1/tls" => Some(Route::Tls),
        "/api/v1/tls/reconcile" => Some(Route::TlsReconcile),
        "/api/v1/tls/renew" => Some(Route::TlsRenew),
        "/api/v1/tls/revoke" => Some(Route::TlsRevoke),
        "/api/v1/tls/delete" => Some(Route::TlsDelete),
        "/api/v1/tls/account/rollover" => Some(Route::TlsAccountRollover),
        "/api/v1/tls/jobs/cancel" => Some(Route::TlsJobCancel),
        "/api/v1/tls/jobs/pause" => Some(Route::TlsJobPause),
        "/api/v1/tls/jobs/resume" => Some(Route::TlsJobResume),
        "/api/v1/audit" => Some(Route::Audit(query)),
        "/api/v1/audit/status" => Some(Route::AuditStatus),
        "/api/v1/events" => Some(Route::Events(EventApiVersion::V1, query)),
        "/api/v1/events/stream" => Some(Route::EventStream(EventApiVersion::V1, query)),
        "/api/v2/events" => Some(Route::Events(EventApiVersion::V2, query)),
        "/api/v2/events/stream" => Some(Route::EventStream(EventApiVersion::V2, query)),
        "/api/v1/process/drain" => Some(Route::ProcessDrain),
        "/api/v1/process/shutdown" => Some(Route::ProcessShutdown),
        _ => None,
    }
}

pub(in super::super) struct EventStreamRoute<'a> {
    pub(in super::super) version: EventApiVersion,
    pub(in super::super) query: Option<&'a str>,
}

pub(in super::super) fn event_stream_route(
    path_and_query: &str,
    accepts_event_stream: bool,
) -> Option<EventStreamRoute<'_>> {
    match match_route(path_and_query)? {
        Route::Events(version, query) if accepts_event_stream => {
            Some(EventStreamRoute { version, query })
        }
        Route::EventStream(version, query) => Some(EventStreamRoute { version, query }),
        _ => None,
    }
}
