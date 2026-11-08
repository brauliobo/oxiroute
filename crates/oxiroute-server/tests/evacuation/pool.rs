use super::*;

#[tokio::test]
async fn evacuating_the_whole_pool_drains_it_and_leaves_requests_running() {
    timeout(TEST_TIMEOUT, async {
        let layout = Layout::new("farm-all", &[Mode::Hold, Mode::Hold]).capacity(1);
        let mut farm = Farm::start(layout).await;
        let on_a = farm.get("all-a");
        farm.origins[0].arrival("all-a").await;
        let on_b = farm.get("all-b");
        farm.origins[1].arrival("all-b").await;

        let report = farm.evacuate_all(false);

        assert_report(&report, 0, 0, &[(LeftRunning::NoDestination, 2)]);
        let health = farm.pool.health_snapshot();
        assert!(
            health
                .endpoints
                .iter()
                .all(|server| server.administrative_state == AdministrativeState::Drain)
        );
        farm.origins[0].release(1);
        farm.origins[1].release(1);
        assert_served_by(&on_a.await.expect("client"), "node-a", "all-a");
        assert_served_by(&on_b.await.expect("client"), "node-b", "all-b");
        assert!(farm.origins[0].seen().is_empty() && farm.origins[1].seen().is_empty());
    })
    .await
    .expect("pool evacuation test timed out");
}

#[tokio::test]
async fn forcing_a_pool_evacuation_cuts_every_request_and_ready_undrains_it() {
    timeout(TEST_TIMEOUT, async {
        let layout = Layout::new("farm-all-forced", &[Mode::Hold, Mode::Hold]).capacity(1);
        let mut farm = Farm::start(layout).await;
        let on_a = farm.get("cut-a");
        farm.origins[0].arrival("cut-a").await;
        let on_b = farm.get("cut-b");
        farm.origins[1].arrival("cut-b").await;

        let report = farm.evacuate_all(true);

        assert_report(&report, 0, 2, &[]);
        farm.origins[0].dropped("cut-a").await;
        farm.origins[1].dropped("cut-b").await;
        for (name, client) in [("cut-a", on_a), ("cut-b", on_b)] {
            let response = client.await.expect("client");
            assert_eq!(response.status, 503, "{}", response.text());
            assert_eq!(response.header("x-oxiroute-reason"), Some("evacuated"));
            assert_eq!(response.header("x-request-id"), Some(name));
        }
        let drained = farm.pool.health_snapshot();
        assert!(
            drained
                .endpoints
                .iter()
                .all(|server| server.administrative_state == AdministrativeState::Drain)
        );
        farm.pool
            .set_administrative_state(AdministrativeState::Ready);
        farm.origins[0].release(1);
        assert_served_by(
            &farm.get("after-ready").await.expect("client"),
            "node-a",
            "after-ready",
        );
    })
    .await
    .expect("forced pool evacuation test timed out");
}

#[tokio::test]
async fn a_pool_queue_holds_the_moved_request_until_a_server_frees() {
    timeout(TEST_TIMEOUT, async {
        let layout = Layout::new("farm-queue", &[Mode::Hold, Mode::Hold])
            .capacity(1)
            .queue();
        let mut farm = Farm::start(layout).await;
        let moved = farm.get("moved-to-queue");
        farm.origins[0].arrival("moved-to-queue").await;
        let busy = farm.get("busy");
        farm.origins[1].arrival("busy").await;

        let report = farm.evacuate("node-a", false);

        assert_report(&report, 1, 0, &[]);
        farm.origins[0].dropped("moved-to-queue").await;
        wait_for_live_phase("moved-to-queue", "queued").await;
        assert!(farm.origins[1].seen().is_empty());
        farm.origins[1].release(2);
        assert_served_by(&busy.await.expect("busy client"), "node-b", "busy");
        farm.origins[1].arrival("moved-to-queue").await;
        assert_served_by(
            &moved.await.expect("moved client"),
            "node-b",
            "moved-to-queue",
        );
    })
    .await
    .expect("queue test timed out");
}

#[tokio::test]
async fn a_request_waiting_in_the_queue_is_not_touched() {
    timeout(TEST_TIMEOUT, async {
        let layout = Layout::new("farm-waiting", &[Mode::Hold, Mode::Hold])
            .capacity(1)
            .queue();
        let mut farm = Farm::start(layout).await;
        let first = farm.get("first");
        farm.origins[0].arrival("first").await;
        let second = farm.get("second");
        farm.origins[1].arrival("second").await;
        let waiting = farm.get("waiting");
        wait_for_live_phase("waiting", "queued").await;

        let report = farm.evacuate("node-a", false);

        assert_report(&report, 1, 0, &[]);
        farm.origins[0].dropped("first").await;
        farm.origins[1].release(3);
        assert_served_by(&second.await.expect("second"), "node-b", "second");
        assert_served_by(&waiting.await.expect("waiting"), "node-b", "waiting");
        assert_served_by(&first.await.expect("first"), "node-b", "first");
    })
    .await
    .expect("waiting request test timed out");
}
