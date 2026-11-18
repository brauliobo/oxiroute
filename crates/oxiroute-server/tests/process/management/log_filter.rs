use serde_json::Value;

use crate::daemon::Daemon;

fn restart_logged_for(log: &str, candidate: &Value) -> bool {
    let candidate = candidate.as_str().expect("candidate revision");
    log.contains(&format!(
        "configuration revision {candidate} is applied except for a setting that only a restart applies"
    ))
}

fn waiting_for_a_restart(generation: &Value) -> bool {
    generation["restartRequired"]["required"] == true
}

#[tokio::test]
async fn the_master_applies_a_log_filter_only_after_the_replacement_commits() {
    let daemon = Daemon::start(true).await;
    let first = daemon.generation().await["activeRevision"].clone();

    let mut refused = daemon.candidate(false, 7);
    refused.log_filter = Some("off".into());
    refused.management.as_mut().expect("management").token_file =
        Some(daemon.path.join("missing.token"));
    daemon.write(&refused);
    daemon.hang_up();
    daemon
        .wait_generation("the replacement to be refused", |generation| {
            generation["lastRejection"] != Value::Null
        })
        .await;
    assert_eq!(daemon.generation().await["activeRevision"], first);

    daemon.write(&daemon.candidate(true, 5));
    daemon.hang_up();
    let applied = daemon
        .wait_generation("the rest of the file to apply", |generation| {
            waiting_for_a_restart(generation) && generation["activeRevision"] != first
        })
        .await;
    let log = daemon.server.stderr();
    assert!(
        restart_logged_for(&log, &applied["restartRequired"]["candidateRevision"]),
        "a refused replacement changed the master's log filter: {log}"
    );

    let mut quiet = daemon.candidate(true, 6);
    quiet.log_filter = Some("off".into());
    daemon.write(&quiet);
    daemon.hang_up();
    let committed = daemon
        .wait_generation("the quiet file to commit", |generation| {
            generation["activeRevision"] != applied["activeRevision"]
        })
        .await;

    let mut silenced = daemon.candidate(true, 9);
    silenced.log_filter = Some("off".into());
    daemon.write(&silenced);
    daemon.hang_up();
    let last = daemon
        .wait_generation("the last file to apply", |generation| {
            waiting_for_a_restart(generation)
                && generation["activeRevision"] != committed["activeRevision"]
        })
        .await;
    let log = daemon.server.stderr();
    assert!(
        !restart_logged_for(&log, &last["restartRequired"]["candidateRevision"]),
        "the committed log filter did not apply to the master: {log}"
    );
}

#[tokio::test]
async fn the_master_and_its_workers_fall_back_to_the_same_environment_filter() {
    let mut daemon = Daemon::launch(true, false, &[("RUST_LOG", "info")]).await;

    let log = daemon
        .server
        .wait_for_stderr("configured management API on")
        .await;

    assert!(log.contains("configured management API on"), "{log}");
}
