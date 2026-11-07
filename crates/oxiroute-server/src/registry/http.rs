use std::net::IpAddr;

use serde::Serialize;
use serde_json::Value;

use super::{
    auth::Principal,
    dto::{
        AdministrativeStateDto, ErrorBody, ErrorDetail, HeartbeatResponse, OperatorStateRequest,
        PoolServerDto, PoolServersResponse, PutResponse, RegistrationDto, RegistrationEntryDto,
        RegistrationsResponse,
    },
    error::RegistryError,
    record::{HeartbeatRequest, RegistrationKey, RegistrationRequest},
    service::{RegistrationView, RegistryService},
};
use crate::AdministrativeState;

pub const MAX_BODY_BYTES: usize = 4096;

/// The secret of an `Authorization: Bearer <secret>` header value.
#[must_use]
pub fn bearer(authorization: &[u8]) -> Option<&[u8]> {
    authorization.strip_prefix(b"Bearer ")
}

/// A registry route and the path parameters it carries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Route<'a> {
    Registrations,
    Servers { pool: &'a str },
    Server { pool: &'a str, name: &'a str },
    Heartbeat { pool: &'a str, name: &'a str },
    State { pool: &'a str, name: &'a str },
}

/// Recognizes the registry routes. Percent signs are refused so no decoding ambiguity exists.
#[must_use]
pub fn match_route(path: &str) -> Option<Route<'_>> {
    if path == "/api/v1/registrations" {
        return Some(Route::Registrations);
    }
    let rest = path.strip_prefix("/api/v1/pools/")?;
    let segments = rest.split('/').collect::<Vec<_>>();
    if segments
        .iter()
        .any(|segment| segment.is_empty() || segment.contains('%'))
    {
        return None;
    }
    match segments.as_slice() {
        [pool, "servers"] => Some(Route::Servers { pool }),
        [pool, "servers", name] => Some(Route::Server { pool, name }),
        [pool, "servers", name, "heartbeat"] => Some(Route::Heartbeat { pool, name }),
        [pool, "servers", name, "state"] => Some(Route::State { pool, name }),
        _ => None,
    }
}

/// Everything a transport needs to know about one request.
pub struct Request<'a> {
    pub method: &'a str,
    pub query: Option<&'a str>,
    pub body: &'a [u8],
    pub principal: &'a Principal,
    pub peer: Option<IpAddr>,
}

/// A transport-neutral reply.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub body: Option<Value>,
    pub allow: Option<&'static str>,
    pub retry_after_seconds: Option<u64>,
    pub close: bool,
}

impl Reply {
    pub(super) fn json(status: u16, body: &impl Serialize) -> Self {
        Self {
            status,
            body: Some(serde_json::to_value(body).expect("registry DTO serializes")),
            allow: None,
            retry_after_seconds: None,
            close: false,
        }
    }

    fn empty(status: u16) -> Self {
        Self {
            status,
            body: None,
            allow: None,
            retry_after_seconds: None,
            close: false,
        }
    }

    #[must_use]
    pub fn error(error: &RegistryError) -> Self {
        let mut reply = Self::json(
            error.status(),
            &ErrorBody {
                error: ErrorDetail {
                    code: error.code().into(),
                    message: error.to_string(),
                },
            },
        );
        match error {
            RegistryError::RateLimited { retry_after } => {
                reply.retry_after_seconds = Some(retry_after.as_secs().max(1));
            }
            RegistryError::GenerationInactive => reply.close = true,
            _ => {}
        }
        reply
    }

    fn method_not_allowed(allow: &'static str) -> Self {
        let mut reply = Self::error(&RegistryError::InvalidRequest("method is not allowed"));
        reply.status = 405;
        if let Some(Value::Object(body)) = &mut reply.body {
            body["error"]["code"] = "method_not_allowed".into();
        }
        reply.allow = Some(allow);
        reply
    }
}

/// Runs one registry request against the service.
#[must_use]
pub fn handle(service: &RegistryService, route: Route<'_>, request: &Request<'_>) -> Reply {
    if request.body.len() > MAX_BODY_BYTES {
        return Reply::error(&RegistryError::BodyTooLarge);
    }
    dispatch(service, route, request).unwrap_or_else(|error| Reply::error(&error))
}

fn dispatch(
    service: &RegistryService,
    route: Route<'_>,
    request: &Request<'_>,
) -> Result<Reply, RegistryError> {
    match (route, request.method) {
        (Route::Registrations, "GET") => registrations(service, request.principal),
        (Route::Servers { pool }, "GET") => pool_servers(service, request.principal, pool, None),
        (Route::Server { pool, name }, "GET") => {
            pool_servers(service, request.principal, pool, Some(name))
        }
        (Route::Server { pool, name }, "PUT") => put(service, request, pool, name),
        (Route::Server { pool, name }, "DELETE") => {
            service.delete(request.principal, pool, name)?;
            Ok(Reply::empty(204))
        }
        (Route::Heartbeat { pool, name }, "POST") => heartbeat(service, request, pool, name),
        (Route::State { pool, name }, "POST") => operator_state(service, request, pool, name),
        (Route::Registrations | Route::Servers { .. }, _) => Ok(Reply::method_not_allowed("GET")),
        (Route::Server { .. }, _) => Ok(Reply::method_not_allowed("GET, PUT, DELETE")),
        (Route::Heartbeat { .. } | Route::State { .. }, _) => Ok(Reply::method_not_allowed("POST")),
    }
}

fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, RegistryError> {
    serde_json::from_slice(body)
        .map_err(|_| RegistryError::InvalidRequest("body must be JSON with only known fields"))
}

fn revision(value: u64) -> String {
    value.to_string()
}

fn put(
    service: &RegistryService,
    request: &Request<'_>,
    pool: &str,
    name: &str,
) -> Result<Reply, RegistryError> {
    let takeover = request
        .query
        .is_some_and(|query| query.split('&').any(|pair| pair == "takeover=true"));
    let body: RegistrationRequest = parse(request.body)?;
    let outcome = service.put(request.principal, pool, name, body, request.peer, takeover)?;
    Ok(Reply::json(
        if outcome.created { 201 } else { 200 },
        &PutResponse {
            server: RegistrationDto::new(&outcome.record, request.principal.can_read()),
            expires_in_ms: outcome.expires_in_ms,
            ttl_ms: outcome.record.ttl_ms,
            registry_revision: revision(outcome.registry_revision),
            warnings: outcome
                .warnings
                .iter()
                .map(|warning| (*warning).to_owned())
                .collect(),
        },
    ))
}

fn heartbeat(
    service: &RegistryService,
    request: &Request<'_>,
    pool: &str,
    name: &str,
) -> Result<Reply, RegistryError> {
    let body: HeartbeatRequest = if request.body.is_empty() {
        HeartbeatRequest::default()
    } else {
        parse(request.body)?
    };
    let ttl = service.heartbeat(request.principal, pool, name, body)?;
    Ok(Reply::json(200, &HeartbeatResponse { expires_in_ms: ttl }))
}

fn operator_state(
    service: &RegistryService,
    request: &Request<'_>,
    pool: &str,
    name: &str,
) -> Result<Reply, RegistryError> {
    let body: OperatorStateRequest = parse(request.body)?;
    service.set_operator_state(
        request.principal,
        pool,
        name,
        match body.state {
            AdministrativeStateDto::Ready => AdministrativeState::Ready,
            AdministrativeStateDto::Drain => AdministrativeState::Drain,
            AdministrativeStateDto::Maintenance => AdministrativeState::Maintenance,
        },
    )?;
    Ok(Reply::empty(204))
}

fn registrations(service: &RegistryService, principal: &Principal) -> Result<Reply, RegistryError> {
    principal.check_read()?;
    let status = service.status();
    Ok(Reply::json(
        200,
        &RegistrationsResponse {
            registrations: service
                .list(None)?
                .iter()
                .map(|view| RegistrationEntryDto::new(view, true))
                .collect(),
            registry_revision: revision(status.revision),
            writer: status.writer,
            degraded: status.degraded,
        },
    ))
}

fn pool_servers(
    service: &RegistryService,
    principal: &Principal,
    pool: &str,
    only: Option<&str>,
) -> Result<Reply, RegistryError> {
    principal.check_read()?;
    let snapshot = service
        .pool_snapshot(pool)
        .ok_or(RegistryError::PoolNotFound)?;
    let views = service.list(Some(pool))?;
    let view_of = |name: &str| -> Option<&RegistrationView> {
        views
            .iter()
            .find(|view| view.record.key() == RegistrationKey::new(pool, name))
    };
    let servers = snapshot
        .endpoints
        .iter()
        .filter(|member| only.is_none_or(|name| member.name == name))
        .map(|member| {
            let view =
                view_of(&member.name).filter(|_| member.origin == crate::ServerOrigin::Dynamic);
            PoolServerDto::new(member, view, true)
        })
        .collect::<Vec<_>>();
    if only.is_some() && servers.is_empty() {
        return Err(RegistryError::RegistrationNotFound);
    }
    Ok(Reply::json(
        200,
        &PoolServersResponse {
            pool: pool.to_owned(),
            servers,
            registry_revision: revision(service.status().revision),
        },
    ))
}

#[cfg(test)]
mod tests;
