use std::sync::atomic::{AtomicBool, Ordering};

use oxiroute_supervisor_process::build_id_of;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;

/// Clients that connect and echo in a loop, so that a connection the hand-over loses is seen.
struct Traffic {
    stop: Arc<AtomicBool>,
    clients: Vec<JoinHandle<Result<u64, String>>>,
}

impl Traffic {
    fn start(listener: SocketAddr, clients: usize) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let clients = (0..clients)
            .map(|_| tokio::spawn(client_loop(listener, Arc::clone(&stop))))
            .collect();
        Self { stop, clients }
    }

    async fn finish(self, daemon: &Daemon) -> u64 {
        self.stop.store(true, Ordering::Release);
        let mut served = 0;
        for client in self.clients {
            served += client
                .await
                .expect("traffic client")
                .unwrap_or_else(|error| {
                    panic!("a connection was lost: {error}\n{}", daemon.server.stderr())
                });
        }
        served
    }
}

async fn client_loop(listener: SocketAddr, stop: Arc<AtomicBool>) -> Result<u64, String> {
    let mut served = 0;
    while !stop.load(Ordering::Acquire) {
        let mut stream = TcpStream::connect(listener)
            .await
            .map_err(|error| format!("connect: {error}"))?;
        for round in 0..3_u8 {
            let payload = [b'a' + round; 16];
            stream
                .write_all(&payload)
                .await
                .map_err(|error| format!("write: {error}"))?;
            let mut response = [0_u8; 16];
            tokio::time::timeout(WIRE_TIMEOUT, stream.read_exact(&mut response))
                .await
                .map_err(|_| "the echo timed out".to_owned())?
                .map_err(|error| format!("read: {error}"))?;
            if response != payload {
                return Err("the echo differs".into());
            }
        }
        served += 1;
    }
    Ok(served)
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

async fn master_build(daemon: &Daemon) -> Value {
    daemon.status().await["masterBuildId"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upgrade_hands_the_listeners_to_the_new_build_without_a_lost_connection() {
    let _guard = process_test_guard().await;
    let mut daemon = Daemon::start(Retirement::default()).await;
    let build = master_build(&daemon).await;
    let mut held = daemon.open_relay().await;
    let traffic = Traffic::start(daemon.listener, 4);

    let image = another_build_of(
        &daemon.binary,
        build.as_str().expect("master build id"),
        0xff,
    );
    daemon.install_executable(&image);
    let upgraded = daemon.upgrade(&[]).await;
    assert!(upgraded.status.success(), "{}", stderr(&upgraded));
    let report = stdout(&upgraded);
    assert!(report.contains("starting "), "{report}");
    assert!(
        report.contains("serving: the successor master "),
        "{report}"
    );
    daemon.adopt_successor(&upgraded);

    let new_build = wait_until("the successor serves the management API", || async {
        let current = master_build(&daemon).await;
        (current != build && !current.is_null()).then_some(current)
    })
    .await;
    assert_eq!(
        new_build.as_str(),
        build_id_of(&daemon.binary).ok().as_deref()
    );
    assert_echo(&mut held, b"held across the hand-over").await;
    assert!(traffic.finish(&daemon).await > 0);

    drop(held);
    assert!(daemon.wait_for_master_exit().await.success());
    assert_echo(&mut daemon.open_relay().await, b"served by the successor").await;
    daemon.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_successor_that_does_not_start_leaves_the_running_master_serving() {
    let _guard = process_test_guard().await;
    let mut daemon = Daemon::start(Retirement::default()).await;
    let build = master_build(&daemon).await;
    let mut held = daemon.open_relay().await;
    let traffic = Traffic::start(daemon.listener, 2);

    daemon.install_executable(b"#!/bin/sh\nexit 3\n");
    let upgraded = daemon.upgrade(&[]).await;
    assert!(!upgraded.status.success());
    let failure = stderr(&upgraded);
    assert!(failure.contains("the successor exited"), "{failure}");

    assert_eq!(master_build(&daemon).await, build);
    assert_echo(&mut held, b"still served").await;
    assert!(traffic.finish(&daemon).await > 0);
    assert!(daemon.server.exit_status().is_none());
    daemon.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_successor_hands_over_again_and_the_drained_master_is_reported_with_wait() {
    let _guard = process_test_guard().await;
    let mut daemon = Daemon::start(Retirement::default()).await;
    let build = master_build(&daemon).await;
    let image = another_build_of(
        &daemon.binary,
        build.as_str().expect("master build id"),
        0xff,
    );
    daemon.install_executable(&image);
    let first = daemon.upgrade(&["--wait"]).await;
    assert!(first.status.success(), "{}", stderr(&first));
    let report = stdout(&first);
    assert!(report.contains("drained: "), "{report}");
    daemon.adopt_successor(&first);
    let second_build = master_build(&daemon).await;
    assert_ne!(second_build, build);

    let image = another_build_of(
        &daemon.binary,
        second_build.as_str().expect("master build id"),
        0x0f,
    );
    daemon.install_executable(&image);
    let second = daemon.upgrade(&["--wait"]).await;
    assert!(second.status.success(), "{}", stderr(&second));
    daemon.adopt_successor(&second);
    wait_until("the second successor serves", || async {
        let current = master_build(&daemon).await;
        (current != second_build && !current.is_null()).then_some(current)
    })
    .await;

    assert_echo(&mut daemon.open_relay().await, b"served by the last").await;
    daemon.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_service_unit_that_cannot_change_its_main_process_refuses_the_upgrade() {
    let _guard = process_test_guard().await;
    let daemon = Daemon::start_in_environment(
        Retirement::default(),
        &[("INVOCATION_ID", "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f")],
    )
    .await;
    let upgraded = daemon.upgrade(&[]).await;
    assert!(!upgraded.status.success());
    assert!(
        stderr(&upgraded).contains("NotifyAccess=main"),
        "{}",
        stderr(&upgraded)
    );
    assert_echo(&mut daemon.open_relay().await, b"still served").await;
    daemon.stop();
}

#[tokio::test]
async fn an_upgrade_without_a_master_says_so() {
    let directory = TempDir::new().expect("configuration directory");
    let config = directory.path().join("oxiroute.kdl");
    fs::write(&config, "").expect("configuration file");
    let output = Command::new(env!("CARGO_BIN_EXE_oxiroute"))
        .arg("upgrade")
        .arg("--config")
        .arg(&config)
        .output()
        .expect("run oxiroute upgrade");
    assert_eq!(output.status.code(), Some(4), "{}", stderr(&output));
    assert!(stderr(&output).contains("no oxiroute master serves"));
}
