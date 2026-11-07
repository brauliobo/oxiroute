#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct ConfiguredCompressionBuilder {
    pub(super) gzip: Arc<HttpGzipPlan>,
}

impl HttpModuleBuilder for ConfiguredCompressionBuilder {
    fn init(&self) -> Module {
        Box::new(ConfiguredCompression {
            gzip: Arc::clone(&self.gzip),
            inner: ResponseCompressionCtx::new_for_algorithm(
                Algorithm::Gzip,
                self.gzip.level,
                false,
                false,
            )
            .with_minimum_compression_bytes(self.gzip.min_length_bytes)
            .with_content_type_filtering(false)
            .with_vary_header(self.gzip.vary),
            ready: false,
        })
    }

    fn order(&self) -> i16 {
        i16::MIN / 2
    }
}

struct ConfiguredCompression {
    gzip: Arc<HttpGzipPlan>,
    inner: ResponseCompressionCtx,
    ready: bool,
}

#[async_trait]
impl HttpModule for ConfiguredCompression {
    async fn request_header_filter(
        &mut self,
        request: &mut pingora::http::RequestHeader,
    ) -> pingora::Result<()> {
        let request_eligible = (match self.gzip.min_http_version {
            HttpGzipMinimumVersion::Http10 => request.version != http::Version::HTTP_09,
            HttpGzipMinimumVersion::Http11 => !matches!(
                request.version,
                http::Version::HTTP_09 | http::Version::HTTP_10
            ),
        }) && (!self.gzip.disable_on_via
            || !request.headers.contains_key("via"));
        if request_eligible {
            self.inner.request_filter(request);
        }
        Ok(())
    }

    async fn response_header_filter(
        &mut self,
        response: &mut pingora::http::ResponseHeader,
        end_of_stream: bool,
    ) -> pingora::Result<()> {
        if !gzip_matches(&self.gzip, response) {
            self.ready = false;
            return Ok(());
        }
        self.inner.response_header_filter(response, end_of_stream);
        self.ready = !response.status.is_informational();
        Ok(())
    }

    fn response_body_filter(
        &mut self,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
    ) -> pingora::Result<()> {
        if !self.ready {
            return Ok(());
        }
        if let Some(compressed) = self
            .inner
            .response_body_filter(body.as_ref(), end_of_stream)
        {
            *body = Some(compressed);
        }
        Ok(())
    }

    fn response_done_filter(&mut self) -> pingora::Result<Option<Bytes>> {
        if !self.ready {
            return Ok(None);
        }
        Ok(self.inner.response_body_filter(None, true))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

pub(super) fn gzip_matches(gzip: &HttpGzipPlan, response: &pingora::http::ResponseHeader) -> bool {
    if !matches!(
        response.status,
        http::StatusCode::OK | http::StatusCode::FORBIDDEN | http::StatusCode::NOT_FOUND
    ) || response.headers.contains_key(CONTENT_ENCODING)
    {
        return false;
    }
    response
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .is_some_and(|content_type| {
            gzip.content_types
                .iter()
                .any(|configured| configured.eq_ignore_ascii_case(content_type))
        })
}
