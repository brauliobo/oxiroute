//! The HTTP/3 transport options reach the QUIC endpoint: every combination serves a multi-megabyte body intact.
use super::origins::{Daemon, draft};
#[allow(clippy::wildcard_imports)]
use super::*;
use oxiroute_config::CongestionControl;

const BODY_BYTES: usize = 4 * 1024 * 1024;

async fn downloaded_with(control: Option<CongestionControl>, offload: Option<bool>) -> u16 {
    let directory = tempfile::tempdir().expect("static directory");
    let body: Vec<u8> = (0..BODY_BYTES)
        .map(|index| u8::try_from(index % 251).expect("below 251"))
        .collect();
    fs::write(directory.path().join("body.bin"), &body).expect("static file");
    let listener = reserve_udp_address();
    let mut config = draft(
        listener,
        static_service(directory.path().to_path_buf()),
        Vec::new(),
    );
    config.http3_congestion_control = control;
    config.http3_segmentation_offload = offload;
    let config = config
        .validate()
        .expect("valid reverse HTTP/3 configuration");
    let mut daemon = Daemon::start(listener, &config).await;
    let answer = daemon.get("/body.bin").await;
    daemon.finish().await;
    assert_eq!(answer.body, body.as_slice());
    answer.status
}

#[tokio::test]
pub(super) async fn bbr_with_segmentation_offload_is_the_default_transport() {
    assert_eq!(downloaded_with(None, None).await, 200);
}

#[tokio::test]
pub(super) async fn cubic_serves_a_large_body() {
    assert_eq!(
        downloaded_with(Some(CongestionControl::Cubic), None).await,
        200
    );
}

#[tokio::test]
pub(super) async fn disabled_segmentation_offload_serves_a_large_body() {
    assert_eq!(downloaded_with(None, Some(false)).await, 200);
}

#[tokio::test]
pub(super) async fn cubic_without_segmentation_offload_serves_a_large_body() {
    assert_eq!(
        downloaded_with(Some(CongestionControl::Cubic), Some(false)).await,
        200
    );
}
