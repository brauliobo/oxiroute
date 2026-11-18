#[allow(clippy::wildcard_imports)]
use super::*;
use crate::rtmp::api::{
    dto::{MediaTicketRequest, TicketResponse, VodTicketRequest},
    ticket::{self, DEFAULT_TTL_SECONDS, MAX_TTL_SECONDS, Scope, TicketError},
};

impl RtmpManagementApi {
    pub(super) fn media_access<'a>(
        &self,
        scope: &Scope<'_>,
        method: &str,
        path: &str,
        path_and_query: &str,
        session: &'a ServerSession,
    ) -> Result<Option<&'a str>, ApiResponse> {
        let request = session.req_header();
        let ticket = if request.headers.contains_key(AUTHORIZATION) {
            None
        } else {
            ticket::query_ticket(request.uri.query()).map_err(ticket_error)?
        };
        let Some(ticket) = ticket else {
            return self
                .authentication_error(method, path, path_and_query, session)
                .map_or(Ok(None), Err);
        };
        self.verify_ticket(scope, ticket, system_time_ms()?)?;
        Ok(Some(ticket))
    }

    pub(super) fn verify_ticket(
        &self,
        scope: &Scope<'_>,
        ticket: &str,
        now_unix_ms: u64,
    ) -> Result<(), ApiResponse> {
        self.tickets
            .verify(scope, ticket, now_unix_ms)
            .map_err(ticket_error)
    }

    pub(super) async fn mint_media_ticket(
        &self,
        method: &str,
        session: &mut ServerSession,
    ) -> ApiResponse {
        if method != "POST" {
            return ApiResponse::method_not_allowed("POST");
        }
        let request: MediaTicketRequest = match ticket_request(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let path = session.req_header().uri.path().to_owned();
        let route = media::match_route(&path).expect("registered media ticket route");
        let Some(entry) = self
            .media_catalog()
            .entry_object(route.service, route.application)
        else {
            return ApiResponse::error(
                404,
                "media_not_found",
                "the application has no HLS or DASH output",
            );
        };
        let object_path = format!(
            "/api/v1/rtmp/media/{}/{}/{}/{entry}",
            route.service, route.application, route.stream
        );
        self.minted(Scope::from(&route), &object_path, request.ttl_seconds)
    }

    pub(super) async fn mint_vod_ticket(
        &self,
        method: &str,
        session: &mut ServerSession,
    ) -> ApiResponse {
        if method != "POST" {
            return ApiResponse::method_not_allowed("POST");
        }
        let request: VodTicketRequest = match ticket_request(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let path = session.req_header().uri.path().to_owned();
        let route = vod::match_route(&path).expect("registered VOD ticket route");
        if !vod::is_ticketable_path(&request.path) {
            return ApiResponse::error(400, "vod_invalid_path", "the VOD path is invalid");
        }
        let source_exists = self
            .vod_catalog()
            .application(route.service, route.application)
            .is_some_and(|application| application.has_source(route.source));
        if !source_exists {
            return ApiResponse::error(404, "vod_not_found", "the VOD source does not exist");
        }
        let scope = Scope::Vod {
            service: route.service,
            application: route.application,
            source: route.source,
            path: &request.path,
        };
        let object_path = format!(
            "/api/v1/rtmp/vod/{}/{}/{}/{}",
            route.service, route.application, route.source, request.path
        );
        self.minted(scope, &object_path, request.ttl_seconds)
    }

    pub(super) async fn mint_recording_ticket(
        &self,
        method: &str,
        session: &mut ServerSession,
    ) -> ApiResponse {
        if method != "POST" {
            return ApiResponse::method_not_allowed("POST");
        }
        let request: MediaTicketRequest = match ticket_request(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let path = session.req_header().uri.path().to_owned();
        let id = recordings::match_ticket_route(&path).expect("registered recording ticket route");
        let catalog = self.recording_catalog();
        let owned_id = id.to_owned();
        match tokio::task::spawn_blocking(move || recordings::ensure_exists(&catalog, &owned_id))
            .await
        {
            Ok(Ok(())) => {}
            Ok(Err(response)) => return response,
            Err(_) => return recordings::storage_unavailable(),
        }
        let object_path = format!("/api/v1/rtmp/recordings/{id}/content");
        self.minted(Scope::Recording { id }, &object_path, request.ttl_seconds)
    }

    fn minted(&self, scope: Scope<'_>, object_path: &str, ttl_seconds: Option<u32>) -> ApiResponse {
        let ttl_seconds = ttl_seconds.unwrap_or(DEFAULT_TTL_SECONDS);
        if !(1..=MAX_TTL_SECONDS).contains(&ttl_seconds) {
            return ApiResponse::error(
                400,
                "invalid_ticket_ttl",
                format!("ttlSeconds must be 1 to {MAX_TTL_SECONDS}"),
            );
        }
        let now_unix_ms = match system_time_ms() {
            Ok(now_unix_ms) => now_unix_ms,
            Err(response) => return response,
        };
        let expires_at_unix_ms = now_unix_ms + u64::from(ttl_seconds) * 1_000;
        let ticket = self.tickets.mint(&scope, expires_at_unix_ms);
        ApiResponse::dto(
            200,
            &TicketResponse {
                path: ticket::ticket_path(object_path, &ticket),
                ticket,
                expires_at_unix_ms,
                ttl_seconds,
            },
        )
        .with_ticket_privacy()
    }

    fn media_catalog(&self) -> Arc<oxiroute_rtmp::MediaCatalog> {
        self.generations
            .as_ref()
            .and_then(GenerationManager::active)
            .map_or_else(
                || self.control.media_catalog(),
                |generation| generation.rtmp_media_catalog(),
            )
    }

    fn vod_catalog(&self) -> Arc<oxiroute_rtmp::VodCatalog> {
        self.generations
            .as_ref()
            .and_then(GenerationManager::active)
            .map_or_else(
                || self.control.vod_catalog(),
                |generation| generation.rtmp_vod_catalog(),
            )
    }
}

async fn ticket_request<T: for<'de> serde::Deserialize<'de>>(
    session: &mut ServerSession,
) -> Result<T, ApiResponse> {
    let body = config::read_config_body(session).await?;
    let json = if body.is_empty() {
        b"{}".to_vec()
    } else {
        body
    };
    serde_json::from_slice(&json)
        .map_err(|_| ApiResponse::error(400, "invalid_json", "request body is invalid JSON"))
}

fn ticket_error(error: TicketError) -> ApiResponse {
    match error {
        TicketError::Expired => {
            ApiResponse::error(401, "ticket_expired", "the media ticket expired")
        }
        TicketError::Malformed | TicketError::Invalid => ApiResponse::error(
            401,
            "invalid_ticket",
            "the media ticket is not valid for this object",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_and_forged_tickets_are_distinct_unauthorized_errors() {
        let key = ticket::TicketKey::generate();
        let scope = Scope::Stream {
            service: "live",
            application: "cam",
            stream: "front",
        };
        let ticket = key.mint(&scope, 1_000);
        let expired = ticket_error(key.verify(&scope, &ticket, 1_000).unwrap_err());
        assert_eq!(expired.status, 401);
        assert!(String::from_utf8_lossy(&expired.body).contains("ticket_expired"));
        let forged = ticket_error(key.verify(&scope, "1000.x", 0).unwrap_err());
        assert_eq!(forged.status, 401);
        assert!(String::from_utf8_lossy(&forged.body).contains("invalid_ticket"));
    }
}
