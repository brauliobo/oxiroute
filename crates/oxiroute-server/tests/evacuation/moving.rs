use super::*;

#[tokio::test]
async fn a_held_get_moves_to_another_server_with_the_same_request_id() {
    timeout(TEST_TIMEOUT, async {
        let mut farm =
            Farm::start(Layout::new("farm-get", &[Mode::Hold, Mode::Reply]).access_log()).await;
        let client = farm.get("moved-get");
        farm.origins[0].arrival("moved-get").await;

        let report = farm.evacuate("node-a", false);

        assert_report(&report, 1, 0, &[]);
        farm.origins[0].dropped("moved-get").await;
        farm.origins[1].arrival("moved-get").await;
        assert_served_by(&client.await.expect("client"), "node-b", "moved-get");
        let health = farm.pool.health_snapshot();
        assert_eq!(
            health.endpoints[0].administrative_state,
            AdministrativeState::Drain
        );
        let event = &farm.access_events(1).await[0];
        assert_eq!(event["requestId"], "moved-get");
        assert_eq!(event["status"], 200);
        assert_eq!(event["servers"], "node-a,node-b");
        assert_eq!(event["attempts"], 1, "the move is not a try");
        assert_eq!(event["evacuations"], 1);
        assert_eq!(event["reason"], "");
    })
    .await
    .expect("moved get test timed out");
}

#[tokio::test]
async fn a_post_with_an_idempotency_key_moves_with_its_whole_body() {
    timeout(TEST_TIMEOUT, async {
        let mut farm =
            Farm::start(Layout::new("farm-post", &[Mode::Hold, Mode::Reply]).replayable_posts())
                .await;
        let client = farm.request(
            "POST",
            "moved-post",
            &[("Idempotency-Key", "k-1")],
            BODY.as_bytes(),
        );
        assert_eq!(farm.origins[0].arrival("moved-post").await, BODY);

        let report = farm.evacuate("node-a", false);

        assert_report(&report, 1, 0, &[]);
        assert_eq!(farm.origins[1].arrival("moved-post").await, BODY);
        assert_served_by(&client.await.expect("client"), "node-b", "moved-post");
    })
    .await
    .expect("moved post test timed out");
}

#[tokio::test]
async fn the_drain_outlives_the_evacuation_and_ready_ends_it() {
    timeout(TEST_TIMEOUT, async {
        let mut farm = Farm::start(Layout::new("farm-drain", &[Mode::Hold, Mode::Reply])).await;
        let moved = farm.get("drained-first");
        farm.origins[0].arrival("drained-first").await;
        farm.evacuate("node-a", false);
        farm.origins[0].dropped("drained-first").await;
        assert_served_by(&moved.await.expect("moved"), "node-b", "drained-first");

        assert_served_by(
            &farm.get("while-drained").await.expect("client"),
            "node-b",
            "while-drained",
        );
        farm.pool
            .set_server_administrative_state("node-a", AdministrativeState::Ready)
            .expect("ready");
        farm.origins[0].release(1);
        assert_served_by(
            &farm.get("after-ready").await.expect("client"),
            "node-a",
            "after-ready",
        );
    })
    .await
    .expect("drain persistence test timed out");
}
