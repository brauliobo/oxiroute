use super::*;

#[tokio::test]
async fn a_response_already_streaming_finishes_on_its_server() {
    timeout(TEST_TIMEOUT, async {
        let mut farm = Farm::start(Layout::new("farm-stream", &[Mode::Stream, Mode::Reply])).await;
        let mut client = TcpStream::connect(farm.address).await.expect("client connect");
        client
            .write_all(b"GET /render HTTP/1.1\r\nHost: t\r\nX-Request-Id: streaming\r\nConnection: close\r\n\r\n")
            .await
            .expect("client request");
        let mut received = Vec::new();
        while !received.ends_with(b"hello") {
            let mut buffer = [0; 256];
            let read = client.read(&mut buffer).await.expect("client read");
            assert_ne!(read, 0, "closed before the first bytes");
            received.extend_from_slice(&buffer[..read]);
        }

        let report = farm.evacuate("node-a", false);

        assert_report(&report, 0, 0, &[(LeftRunning::ResponseStarted, 1)]);
        farm.origins[0].release(1);
        client.read_to_end(&mut received).await.expect("client finish");
        assert!(received.ends_with(b"helloworld"), "{}", String::from_utf8_lossy(&received));
        assert!(farm.origins[1].seen().is_empty());
    })
    .await
    .expect("streaming test timed out");
}

#[tokio::test]
async fn forcing_resets_a_response_that_already_started() {
    timeout(TEST_TIMEOUT, async {
        let mut farm = Farm::start(Layout::new("farm-reset", &[Mode::Stream, Mode::Reply])).await;
        let mut client = TcpStream::connect(farm.address).await.expect("client connect");
        client
            .write_all(b"GET /render HTTP/1.1\r\nHost: t\r\nX-Request-Id: reset\r\nConnection: close\r\n\r\n")
            .await
            .expect("client request");
        let mut received = Vec::new();
        while !received.ends_with(b"hello") {
            let mut buffer = [0; 256];
            let read = client.read(&mut buffer).await.expect("client read");
            assert_ne!(read, 0, "closed before the first bytes");
            received.extend_from_slice(&buffer[..read]);
        }

        let report = farm.evacuate("node-a", true);

        assert_report(&report, 0, 1, &[]);
        farm.origins[0].arrival("reset").await;
        farm.origins[0].dropped("reset").await;
        let _ = client.read_to_end(&mut received).await;
        assert!(!received.ends_with(b"world"), "{}", String::from_utf8_lossy(&received));
        assert!(farm.origins[1].seen().is_empty());
    })
    .await
    .expect("forced reset test timed out");
}

#[tokio::test]
async fn a_post_without_an_idempotency_key_stays_unless_forced() {
    timeout(TEST_TIMEOUT, async {
        let mut farm =
            Farm::start(Layout::new("farm-keyless", &[Mode::Hold, Mode::Reply]).replayable_posts())
                .await;
        let stays = farm.request("POST", "keyless", &[], BODY.as_bytes());
        farm.origins[0].arrival("keyless").await;

        let report = farm.evacuate("node-a", false);

        assert_report(&report, 0, 0, &[(LeftRunning::NotReplayable, 1)]);
        farm.origins[0].release(1);
        assert_served_by(&stays.await.expect("client"), "node-a", "keyless");
        assert!(farm.origins[0].seen().is_empty() && farm.origins[1].seen().is_empty());
    })
    .await
    .expect("keyless post test timed out");
}

#[tokio::test]
async fn forcing_a_post_without_an_idempotency_key_answers_the_evacuated_error() {
    timeout(TEST_TIMEOUT, async {
        let mut farm =
            Farm::start(Layout::new("farm-killed", &[Mode::Hold, Mode::Reply]).replayable_posts())
                .await;
        let client = farm.request("POST", "killed", &[], BODY.as_bytes());
        farm.origins[0].arrival("killed").await;

        let report = farm.evacuate("node-a", true);

        assert_report(&report, 0, 1, &[]);
        farm.origins[0].dropped("killed").await;
        let response = client.await.expect("client");
        assert_eq!(response.status, 503, "{}", response.text());
        assert_eq!(response.header("x-oxiroute-reason"), Some("evacuated"));
        assert_eq!(response.header("x-request-id"), Some("killed"));
        assert!(
            !response.text().contains("node-"),
            "no server name reaches the caller"
        );
        assert!(farm.origins[1].seen().is_empty());
    })
    .await
    .expect("forced post test timed out");
}

#[tokio::test]
async fn the_only_server_keeps_its_requests() {
    timeout(TEST_TIMEOUT, async {
        let mut farm = Farm::start(Layout::new("farm-alone", &[Mode::Hold])).await;
        let client = farm.get("alone");
        farm.origins[0].arrival("alone").await;

        let report = farm.evacuate("node-a", false);

        assert_report(&report, 0, 0, &[(LeftRunning::NoDestination, 1)]);
        farm.origins[0].release(1);
        assert_served_by(&client.await.expect("client"), "node-a", "alone");
        assert!(farm.origins[0].seen().is_empty());
    })
    .await
    .expect("only server test timed out");
}

#[tokio::test]
async fn forcing_cuts_the_requests_of_the_only_server() {
    timeout(TEST_TIMEOUT, async {
        let mut farm = Farm::start(Layout::new("farm-alone-forced", &[Mode::Hold])).await;
        let client = farm.get("alone-forced");
        farm.origins[0].arrival("alone-forced").await;

        let report = farm.evacuate("node-a", true);

        assert_report(&report, 0, 1, &[]);
        farm.origins[0].dropped("alone-forced").await;
        let response = client.await.expect("client");
        assert_eq!(response.status, 503, "{}", response.text());
        assert_eq!(response.header("x-oxiroute-reason"), Some("evacuated"));
        assert_eq!(response.header("x-request-id"), Some("alone-forced"));
    })
    .await
    .expect("forced only server test timed out");
}
