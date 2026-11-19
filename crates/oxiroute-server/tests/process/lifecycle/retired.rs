use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;

fn open_descriptors(pid: u32) -> usize {
    fs::read_dir(format!("/proc/{pid}/fd"))
        .expect("the descriptors of the master")
        .count()
}

fn retired_revisions(generation: &Value) -> Vec<Value> {
    generation["retiredGenerations"]
        .as_array()
        .expect("retired list")
        .iter()
        .map(|entry| entry["revision"].clone())
        .collect()
}

impl Daemon {
    /// Writes the next revision and returns once it is the active one.
    pub(super) async fn apply(&mut self) -> Value {
        let before = self.generation().await["activeRevision"].clone();
        self.write_next_revision();
        wait_until("the new revision activates", || async {
            let generation = self.generation().await;
            (generation["activeRevision"] != before).then_some(generation)
        })
        .await
    }
}

async fn is_closed(stream: &mut TcpStream) -> bool {
    let mut byte = [0_u8; 1];
    matches!(
        tokio::time::timeout(Duration::from_secs(15), stream.read(&mut byte)).await,
        Ok(Ok(0) | Err(_))
    )
}

#[tokio::test]
async fn quick_reloads_keep_every_old_worker_draining_listed_and_leak_nothing() {
    let _guard = process_test_guard().await;
    let mut daemon = Daemon::start(Retirement::default()).await;
    let master = daemon.server.pid();
    let baseline = open_descriptors(master);
    let mut relays = vec![daemon.open_relay().await];
    let mut revisions = vec![daemon.generation().await["activeRevision"].clone()];
    daemon.apply().await;

    for _ in 0..3 {
        relays.push(daemon.open_relay().await);
        revisions.push(daemon.generation().await["activeRevision"].clone());
        let applied = daemon.apply().await;
        assert!(applied["pendingApply"].is_null(), "{applied}");
    }
    let listed = wait_until("every replaced worker is listed, oldest first", || async {
        let generation = daemon.generation().await;
        (retired_revisions(&generation) == revisions).then_some(generation)
    })
    .await;
    for entry in listed["retiredGenerations"].as_array().unwrap() {
        assert_eq!(entry["state"], "draining", "{listed}");
        assert!(entry["deadlineRemainingSeconds"].as_u64().unwrap() > 10_000);
    }
    assert!(open_descriptors(master) > baseline);
    for relay in &mut relays {
        assert_echo(relay, b"still served").await;
    }

    relays.remove(0);
    wait_until("the oldest worker leaves the list", || async {
        let generation = daemon.generation().await;
        (retired_revisions(&generation) == revisions[1..]).then_some(())
    })
    .await;
    relays.clear();
    wait_until("every retired worker is gone", || async {
        let generation = daemon.generation().await;
        retired_revisions(&generation).is_empty().then_some(())
    })
    .await;
    wait_until(
        "the master holds no descriptor of a retired worker",
        || async { (open_descriptors(master) == baseline).then_some(()) },
    )
    .await;
    assert_echo(&mut daemon.open_relay().await, b"served by the newest").await;
    daemon.stop();
}

#[tokio::test]
async fn a_reload_beyond_the_limit_closes_the_oldest_draining_worker_and_says_why() {
    let _guard = process_test_guard().await;
    let mut daemon = Daemon::start(Retirement {
        drain_deadline_ms: None,
        max_retired_generations: 2,
    })
    .await;
    let mut oldest = daemon.open_relay().await;
    daemon.apply().await;
    let mut middle = daemon.open_relay().await;
    daemon.apply().await;
    let mut newest = daemon.open_relay().await;
    let listed = wait_until("both replaced workers are listed", || async {
        let generation = daemon.generation().await;
        (generation["retiredGenerations"].as_array()?.len() == 2).then_some(generation)
    })
    .await;
    assert_eq!(listed["maxRetiredGenerations"], 2);
    assert_echo(&mut oldest, b"within the limit").await;

    daemon.apply().await;
    assert!(is_closed(&mut oldest).await, "the oldest worker was closed");
    assert_echo(&mut middle, b"still draining").await;
    assert_echo(&mut newest, b"still draining").await;
    let log = daemon
        .server
        .wait_for_stderr("closed the oldest replaced worker")
        .await;
    assert!(log.contains("retired_limit"), "{log}");
    wait_until("only the two newest workers are listed", || async {
        let generation = daemon.generation().await;
        (generation["retiredGenerations"].as_array()?.len() == 2).then_some(())
    })
    .await;
    daemon.stop();
}
