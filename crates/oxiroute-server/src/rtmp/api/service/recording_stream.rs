#[allow(clippy::wildcard_imports)]
use super::*;
use crate::rtmp::api::recordings::{self as recording_api, ContentOutcome, RecordingStream};
use std::os::unix::fs::FileExt as _;

const STREAM_CHUNK_BYTES: u64 = 256 * 1024;

impl RtmpManagementApi {
    pub(super) async fn recording_content(
        &self,
        id: &str,
        session: &ServerSession,
    ) -> ContentOutcome {
        let catalog = self.recording_catalog();
        let id = id.to_owned();
        let range = session
            .req_header()
            .headers
            .get("range")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        match tokio::task::spawn_blocking(move || {
            recording_api::content(&catalog, &id, range.as_deref())
        })
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => ContentOutcome::Reply(recording_api::storage_unavailable()),
        }
    }
}

pub(super) async fn write_recording_stream(
    session: &mut ServerSession,
    stream: RecordingStream,
    correlation_id: &str,
    ticketed: bool,
) -> bool {
    let mut builder = Response::builder()
        .status(if stream.partial { 206 } else { 200 })
        .header("content-type", recording_api::content_type())
        .header("content-length", stream.length)
        .header("accept-ranges", "bytes")
        .header("cache-control", "no-store")
        .header("x-correlation-id", correlation_id)
        .header(
            "content-disposition",
            format!("attachment; filename=\"{}\"", stream.filename),
        );
    if ticketed {
        builder = builder.header("referrer-policy", "no-referrer");
    }
    if stream.partial {
        builder = builder.header(
            "content-range",
            format!(
                "bytes {}-{}/{}",
                stream.start,
                stream.start + stream.length - 1,
                stream.total
            ),
        );
    }
    let (parts, ()) = builder
        .body(())
        .expect("recording response headers are valid")
        .into_parts();
    let header: ResponseHeader = parts.into();
    if session
        .write_response_header(Box::new(header))
        .await
        .is_err()
    {
        return false;
    }
    let end = stream.start + stream.length;
    let mut offset = stream.start;
    if offset == end {
        return session.finish_body().await.is_ok();
    }
    while offset < end {
        let length = (end - offset).min(STREAM_CHUNK_BYTES);
        let file = Arc::clone(&stream.file);
        let read = tokio::task::spawn_blocking(move || {
            let mut chunk =
                vec![0; usize::try_from(length).expect("stream chunk fits the address space")];
            file.read_exact_at(&mut chunk, offset).map(|()| chunk)
        })
        .await;
        let Ok(Ok(chunk)) = read else {
            return false;
        };
        offset += length;
        if session
            .write_response_body(chunk.into(), offset == end)
            .await
            .is_err()
        {
            return false;
        }
    }
    true
}
