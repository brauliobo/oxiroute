use super::*;

#[tokio::test]
async fn a_request_is_moved_three_times_and_the_fourth_stays() {
    timeout(TEST_TIMEOUT, async {
        let mut farm = Farm::start(Layout::new("farm-chain", &[Mode::Hold; 4])).await;
        let client = farm.get("chain");
        farm.origins[0].arrival("chain").await;

        for hop in 0..3 {
            let report = farm.evacuate(&server_name(hop), false);
            assert_report(&report, 1, 0, &[]);
            farm.origins[hop].dropped("chain").await;
            farm.origins[hop + 1].arrival("chain").await;
        }
        let report = farm.evacuate("node-d", false);

        assert_report(&report, 0, 0, &[(LeftRunning::LimitReached, 1)]);
        farm.origins[3].release(1);
        assert_served_by(&client.await.expect("client"), "node-d", "chain");
    })
    .await
    .expect("chain test timed out");
}

/// Evacuates at the moment the origin lets the response go: whichever side wins, the caller gets
/// exactly one complete response and the origin that did not answer never sees a second request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn evacuating_while_the_response_header_arrives_never_hangs_or_duplicates() {
    for round in 0..15 {
        timeout(TEST_TIMEOUT, async {
            let mut farm = Farm::start(Layout::new("farm-race", &[Mode::Hold, Mode::Reply])).await;
            let id = format!("race-{round}");
            let client = farm.get(&id);
            farm.origins[0].arrival(&id).await;
            let barrier = Arc::new(Barrier::new(2));
            let gate = Arc::clone(&farm.origins[0].gate);
            let release = {
                let barrier = Arc::clone(&barrier);
                tokio::task::spawn_blocking(move || {
                    barrier.wait();
                    gate.add_permits(1);
                })
            };
            let pool = Arc::clone(&farm.pool);
            let evacuate = tokio::task::spawn_blocking(move || {
                barrier.wait();
                pool.cancel_server_requests("node-a", false)
                    .expect("known server")
            });
            release.await.expect("release");
            let report = evacuate.await.expect("evacuate");

            let response = client.await.expect("client");
            let moved = report.redispatched == 1;
            // A request that already finished is no candidate any more, which is a third valid outcome.
            assert!(
                report.redispatched + report.left_running_total() <= 1,
                "{report:?}"
            );
            assert_served_by(&response, if moved { "node-b" } else { "node-a" }, &id);
            let arrivals = farm.origins[1].seen();
            assert_eq!(arrivals.len(), usize::from(moved), "{arrivals:?}");
        })
        .await
        .expect("race round timed out");
    }
}
