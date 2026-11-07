#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct ForwardCacheCapture {
    pub(super) transaction: Option<CacheTransaction>,
    pub(super) status: StatusCode,
    pub(super) headers: HeaderMap,
    pub(super) body: Vec<u8>,
    pub(super) tags: Vec<Bytes>,
    pub(super) timing: ResponseTiming,
    pub(super) admissible: bool,
}

pub(super) struct ForwardCacheBody<B> {
    pub(super) inner: B,
    pub(super) capture: Option<ForwardCacheCapture>,
}

impl<B> Body for ForwardCacheBody<B>
where
    B: Body<Data = Bytes, Error = BoxError> + Unpin,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_frame(context) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(capture) = &mut this.capture {
                    if let Some(data) = frame.data_ref() {
                        capture.record_data(data);
                    } else {
                        capture.admissible = false;
                    }
                }
                if this.inner.is_end_stream()
                    && let Some(capture) = this.capture.take()
                {
                    tokio::spawn(finish_forward_cache_capture(capture));
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => {
                if let Some(mut capture) = this.capture.take()
                    && let Some(transaction) = capture.transaction.as_mut()
                {
                    transaction.cancel();
                }
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                if let Some(capture) = this.capture.take() {
                    tokio::spawn(finish_forward_cache_capture(capture));
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}

impl<B> Drop for ForwardCacheBody<B> {
    fn drop(&mut self) {
        if let Some(capture) = self.capture.as_mut()
            && let Some(transaction) = capture.transaction.as_mut()
        {
            transaction.cancel();
        }
    }
}

impl ForwardCacheCapture {
    fn record_data(&mut self, data: &Bytes) {
        if !self.admissible {
            return;
        }
        let limit = self
            .transaction
            .as_ref()
            .expect("forward cache capture owns a transaction")
            .max_body_bytes();
        if data.len() > limit.saturating_sub(self.body.len()) {
            self.admissible = false;
            self.body.clear();
            return;
        }
        self.body.extend_from_slice(data);
    }
}

async fn finish_forward_cache_capture(mut capture: ForwardCacheCapture) {
    let Some(transaction) = capture.transaction.as_mut() else {
        return;
    };
    let tags_valid = transaction.cache_tags_within_limits(&capture.tags);
    if !capture.admissible
        || !tags_valid
        || !response_representation_valid_forward(
            capture.status,
            &capture.headers,
            capture.body.len(),
        )
    {
        transaction.complete_without_store();
        return;
    }
    let tag_refs = capture.tags.iter().map(Bytes::as_ref).collect::<Vec<_>>();
    let prepared = transaction.prepare_response(
        capture.status,
        &capture.headers,
        Bytes::from(capture.body),
        capture.timing,
        &tag_refs,
    );
    let Ok(entry) = prepared else {
        transaction.complete_without_store();
        return;
    };
    let _ = transaction.admit(entry).await;
}

pub(super) fn cached_forward_response(
    response: CachedResponse,
    method: &Method,
) -> Response<ForwardProxyBody> {
    let body_forbidden = matches!(
        response.status,
        StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT | StatusCode::NOT_MODIFIED
    );
    let body = if method == Method::HEAD || body_forbidden {
        Bytes::new()
    } else {
        response.body.clone()
    };
    let mut headers = response.headers;
    if matches!(
        response.status,
        StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED
    ) {
        headers.remove(header::CONTENT_LENGTH);
    } else {
        let length = if response.status == StatusCode::RESET_CONTENT {
            0
        } else {
            response.body.len()
        };
        headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&length.to_string()).expect("bounded cache body length"),
        );
    }
    let mut output = Response::builder()
        .status(response.status)
        .body(
            Full::new(body)
                .map_err(|never: Infallible| -> BoxError { match never {} })
                .boxed(),
        )
        .expect("cached forward response");
    *output.headers_mut() = headers;
    output
}

pub(super) fn forward_response_from_parts(
    mut parts: http::response::Parts,
    body: Bytes,
    head: bool,
) -> Response<ForwardProxyBody> {
    if matches!(
        parts.status,
        StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED
    ) {
        parts.headers.remove(header::CONTENT_LENGTH);
    } else if parts.status == StatusCode::RESET_CONTENT {
        parts
            .headers
            .insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
    }
    let body = if head { Bytes::new() } else { body };
    Response::from_parts(
        parts,
        Full::new(body)
            .map_err(|never: Infallible| -> BoxError { match never {} })
            .boxed(),
    )
}

pub(super) fn response_representation_valid_forward(
    status: StatusCode,
    headers: &HeaderMap,
    body_len: usize,
) -> bool {
    if status.is_informational()
        || status == StatusCode::SWITCHING_PROTOCOLS
        || status == StatusCode::NOT_MODIFIED
    {
        return false;
    }
    if matches!(status, StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT) && body_len != 0 {
        return false;
    }
    let mut lengths = headers.get_all(header::CONTENT_LENGTH).iter();
    let Some(length) = lengths.next() else {
        return true;
    };
    lengths.next().is_none()
        && length
            .to_str()
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            == Some(body_len)
}

pub(super) fn response_surrogate_tags_forward(
    headers: &HeaderMap,
    name: &http::HeaderName,
) -> Vec<Bytes> {
    headers
        .get_all(name)
        .iter()
        .flat_map(|value| {
            value
                .as_bytes()
                .split(u8::is_ascii_whitespace)
                .filter(|tag| !tag.is_empty())
                .map(Bytes::copy_from_slice)
        })
        .collect()
}

async fn purge_forward_cache_base(
    plan: &HttpCachePlan,
    request: &CacheRequest,
) -> Result<oxiroute_cache::PurgeResult, CacheBackendError> {
    let base = plan.cache.base(request)?;
    plan.cache.purge_base(&base).await
}

impl ForwardHttp1ServicePlan {
    pub(super) async fn handle_forward_cache_purge(
        &self,
        request: Request<Incoming>,
        target: &oxiroute_forward_proxy::ForwardTarget,
    ) -> Response<ForwardProxyBody> {
        let Some(plan) = &self.cache else {
            return self.rejection(RequestFailure::BadRequest);
        };
        let Some(access) = &plan.purge_access else {
            return self.rejection(RequestFailure::BadRequest);
        };
        if !access.authorizes(request.headers()) {
            let mut response = response(StatusCode::UNAUTHORIZED, Bytes::new());
            response
                .headers_mut()
                .insert(header::PROXY_AUTHENTICATE, access.challenge().clone());
            return response;
        }
        if request
            .body()
            .size_hint()
            .upper()
            .is_some_and(|length| length > 0)
        {
            return response(StatusCode::BAD_REQUEST, Bytes::new());
        }
        let headers = request.headers().clone();
        let cache_request = CacheRequest {
            method: request.method().clone(),
            scheme: target.scheme.as_str(),
            authority: target.destination.authority(),
            path: target.origin_form.path().to_owned(),
            query: target.origin_form.query().map(str::to_owned),
            headers: headers.clone(),
            request_started: plan.cache.now(),
        };
        let result = if let Some(header_name) = &plan.surrogate_header {
            if let Some(value) = headers.get(header_name) {
                let bytes = value.as_bytes();
                if bytes.is_empty()
                    || !bytes
                        .iter()
                        .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b',' | b'"'))
                {
                    return response(StatusCode::BAD_REQUEST, Bytes::new());
                }
                plan.cache.purge_tag(bytes).await
            } else {
                purge_forward_cache_base(plan, &cache_request).await
            }
        } else {
            purge_forward_cache_base(plan, &cache_request).await
        };
        let status = match result {
            Ok(result) if result.entries == 0 => StatusCode::NOT_FOUND,
            Ok(_) => StatusCode::OK,
            Err(error) => {
                log::warn!("forward cache purge failed: {error}");
                if error.is_invalid_request() {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                }
            }
        };
        let mut output = response(status, Bytes::new());
        output.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
        output
    }

    #[allow(clippy::too_many_lines)]
    pub(super) async fn prepare_forward_cache(
        &self,
        request: &Request<Incoming>,
        target: &oxiroute_forward_proxy::ForwardTarget,
        authenticated: bool,
        listener: &crate::ListenerMetrics,
    ) -> ForwardCacheDecision {
        let Some(plan) = &self.cache else {
            return ForwardCacheDecision::Bypass;
        };
        if authenticated
            || !matches!(request.method(), &Method::GET | &Method::HEAD)
            || !plan.allows_method(request.method())
            || request.body().size_hint().upper() != Some(0)
            || request.headers().contains_key(header::AUTHORIZATION)
            || request.headers().contains_key(header::PROXY_AUTHORIZATION)
            || request.headers().contains_key(header::COOKIE)
        {
            return ForwardCacheDecision::Bypass;
        }
        let headers = request.headers().clone();
        let cache_request = CacheRequest {
            method: request.method().clone(),
            scheme: target.scheme.as_str(),
            authority: target.destination.authority(),
            path: target.origin_form.path().to_owned(),
            query: target.origin_form.query().map(str::to_owned),
            headers,
            request_started: plan.cache.now(),
        };
        match CacheTransaction::new(Arc::clone(plan), cache_request, listener.clone())
            .start()
            .await
        {
            CacheStart::Bypass(failure) => {
                if let CacheStartFailure::Lookup(error) = failure
                    && !error.is_invalid_request()
                {
                    log::warn!("forward cache lookup bypassed: {error}");
                }
                ForwardCacheDecision::Bypass
            }
            CacheStart::Hit(cached) => {
                ForwardCacheDecision::Respond(cached_forward_response(cached, request.method()))
            }
            CacheStart::OnlyIfCached => {
                ForwardCacheDecision::Respond(response(StatusCode::GATEWAY_TIMEOUT, Bytes::new()))
            }
            CacheStart::MissLeader(transaction) | CacheStart::RevalidationLeader(transaction) => {
                ForwardCacheDecision::Continue(transaction)
            }
        }
    }

    pub(super) async fn prepare_h3_forward_cache(
        &self,
        request: &Request<()>,
        target: &oxiroute_forward_proxy::ForwardTarget,
        authenticated: bool,
        body_empty: bool,
        listener: &crate::ListenerMetrics,
    ) -> H3ForwardCacheDecision {
        let Some(plan) = &self.cache else {
            return H3ForwardCacheDecision::Bypass;
        };
        if authenticated
            || !matches!(request.method(), &Method::GET | &Method::HEAD)
            || !plan.allows_method(request.method())
            || !body_empty
            || request.headers().contains_key(header::AUTHORIZATION)
            || request.headers().contains_key(header::PROXY_AUTHORIZATION)
            || request.headers().contains_key(header::COOKIE)
            || crate::http_proxy::cache_request_bypasses_cache(request.headers())
        {
            return H3ForwardCacheDecision::Bypass;
        }
        let cache_request = CacheRequest {
            method: request.method().clone(),
            scheme: target.scheme.as_str(),
            authority: target.destination.authority(),
            path: target.origin_form.path().to_owned(),
            query: target.origin_form.query().map(str::to_owned),
            headers: request.headers().clone(),
            request_started: plan.cache.now(),
        };
        match CacheTransaction::new(Arc::clone(plan), cache_request, listener.clone())
            .start()
            .await
        {
            CacheStart::Bypass(failure) => {
                if let CacheStartFailure::Lookup(error) = failure
                    && !error.is_invalid_request()
                {
                    log::warn!("forward HTTP/3 cache lookup bypassed: {error}");
                }
                H3ForwardCacheDecision::Bypass
            }
            CacheStart::Hit(response) => H3ForwardCacheDecision::Respond(Box::new(response)),
            CacheStart::OnlyIfCached => H3ForwardCacheDecision::OnlyIfCached,
            CacheStart::MissLeader(transaction) | CacheStart::RevalidationLeader(transaction) => {
                H3ForwardCacheDecision::Continue(Box::new(transaction))
            }
        }
    }

    pub(super) async fn handle_h3_forward_cache_purge(
        &self,
        request: &Request<()>,
        target: &oxiroute_forward_proxy::ForwardTarget,
        body_empty: bool,
    ) -> H3ForwardResponse {
        let Some(plan) = &self.cache else {
            return h3_forward_status_response(StatusCode::BAD_REQUEST);
        };
        let Some(access) = &plan.purge_access else {
            return h3_forward_status_response(StatusCode::BAD_REQUEST);
        };
        if !access.authorizes(request.headers()) {
            let mut response = h3_forward_status_response(StatusCode::UNAUTHORIZED);
            response
                .headers
                .insert(header::PROXY_AUTHENTICATE, access.challenge().clone());
            return response;
        }
        if !body_empty {
            return h3_forward_status_response(StatusCode::BAD_REQUEST);
        }
        let cache_request = CacheRequest {
            method: request.method().clone(),
            scheme: target.scheme.as_str(),
            authority: target.destination.authority(),
            path: target.origin_form.path().to_owned(),
            query: target.origin_form.query().map(str::to_owned),
            headers: request.headers().clone(),
            request_started: plan.cache.now(),
        };
        let result = if let Some(value) = plan
            .surrogate_header
            .as_ref()
            .and_then(|header| request.headers().get(header))
        {
            plan.cache.purge_tag(value.as_bytes()).await
        } else {
            purge_forward_cache_base(plan, &cache_request).await
        };
        let status = match result {
            Ok(result) if result.entries == 0 => StatusCode::NOT_FOUND,
            Ok(_) => StatusCode::OK,
            Err(error) => {
                log::warn!("forward HTTP/3 cache purge failed: {error}");
                if error.is_invalid_request() {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                }
            }
        };
        let mut response = h3_forward_status_response(status);
        response.headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
        response
    }

    pub(super) async fn forward_failure(
        &self,
        cache_transaction: &mut Option<CacheTransaction>,
        failure: RequestFailure,
        class: ForwardCacheFailure,
    ) -> Response<ForwardProxyBody> {
        if let Some(response) = self.forward_stale_response(cache_transaction, class).await {
            return response;
        }
        if let Some(transaction) = cache_transaction.as_mut() {
            transaction.complete_without_store();
        }
        self.rejection(failure)
    }

    pub(super) async fn forward_stale_response(
        &self,
        cache_transaction: &mut Option<CacheTransaction>,
        failure: ForwardCacheFailure,
    ) -> Option<Response<ForwardProxyBody>> {
        let transaction = cache_transaction.as_mut()?;
        let method = transaction.request().method.clone();
        let response = transaction.stale_response(failure.cache_class()).await?;
        transaction.complete_without_store();
        transaction.record_hit();
        Some(cached_forward_response(response, &method))
    }
}
