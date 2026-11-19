use super::*;

#[tokio::test]
async fn status_shows_the_master_and_worker_build_ids() {
    let _guard = process_test_guard().await;
    let daemon = Daemon::start(Retirement::default()).await;
    let status = daemon.status().await;
    let worker = status["buildId"].as_str().expect("worker build id");
    assert!(worker.starts_with("gnu:"), "{status}");
    assert_eq!(status["masterBuildId"], status["buildId"], "{status}");
    daemon.stop();
}

#[tokio::test]
async fn a_binary_replaced_under_the_master_is_refused_until_it_is_upgraded() {
    let _guard = process_test_guard().await;
    let mut daemon = Daemon::start(Retirement::default()).await;
    let mut relay = daemon.open_relay().await;
    let before = daemon.generation().await["activeRevision"].clone();
    let build = daemon.status().await["masterBuildId"].clone();

    let original = daemon.directory.path().join("oxiroute.original");
    fs::rename(&daemon.binary, &original).expect("set the original aside");
    let changed = another_build_of(&original, build.as_str().expect("master build id"), 0xff);
    daemon.install_executable(&changed);

    daemon.write_next_revision();
    let rejected = wait_until("the reload is refused", || async {
        let generation = daemon.generation().await;
        generation["lastRejection"]
            .is_object()
            .then_some(generation)
    })
    .await;
    let detail = rejected["lastRejection"].to_string();
    assert!(detail.contains("is not the master build"), "{detail}");
    assert!(detail.contains("oxiroute upgrade"), "{detail}");
    assert_eq!(
        rejected["activeRevision"], before,
        "the old worker keeps serving"
    );
    assert_eq!(daemon.status().await["masterBuildId"], build);
    assert_echo(&mut relay, b"still served").await;

    fs::rename(&original, &daemon.binary).expect("restore the executable");
    daemon.signal_master("HUP");
    let applied = wait_until("the reload applies with the master's own build", || async {
        let generation = daemon.generation().await;
        (generation["activeRevision"] != before).then_some(generation)
    })
    .await;
    assert!(applied["lastRejection"].is_null(), "{applied}");
    assert_echo(&mut relay, b"served across the reload").await;
    daemon.stop();
}
