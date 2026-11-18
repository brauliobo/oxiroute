//! `events follow`, `audit list` and `audit tail` against a real daemon, and the reconnect of `events follow`.

use std::{
    io::{BufRead as _, BufReader, Write as _},
    os::unix::net::UnixListener,
    process::{Child, Command, Output, Stdio},
    time::Duration,
};

use serde_json::Value;
use tempfile::TempDir;
use tokio::{task::spawn_blocking, time::timeout};

use super::{Daemon, cli_command, stderr, stdout};

const LIMIT: Duration = Duration::from_secs(30);
const EVACUATION: [&str; 2] = ["pool_evacuate", "server_evacuation"];

fn spawn(daemon: &Daemon, words: &[&str]) -> Child {
    cli_command()
        .args(daemon.connection())
        .args(words)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("oxiroute CLI")
}

async fn finish(child: Child) -> Output {
    let pid = child.id();
    if let Ok(output) = timeout(LIMIT, spawn_blocking(move || child.wait_with_output())).await {
        return output.expect("join").expect("output");
    }
    Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .expect("kill");
    panic!("the command did not finish within {LIMIT:?}");
}

fn evacuate(daemon: &Daemon) {
    let output = daemon.run(&["pool", "evacuate", "farm"]);
    assert!(output.status.success(), "{}", stderr(&output));
}

fn latest_event_cursor(daemon: &Daemon) -> u64 {
    let listing = daemon.run(&["--output", "json", "events", "list", "--limit", "1000"]);
    serde_json::from_slice::<Value>(&listing.stdout).expect("JSON")["cursor"]
        .as_u64()
        .expect("cursor")
}

fn json_lines(output: &Output) -> Vec<Value> {
    stdout(output)
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect()
}

#[tokio::test]
async fn events_follow_prints_the_events_after_a_cursor_and_stops_at_the_count() {
    let daemon = Daemon::start().await;
    let cursor = latest_event_cursor(&daemon);
    evacuate(&daemon);
    evacuate(&daemon);

    let child = spawn(
        &daemon,
        &[
            "--output",
            "json",
            "events",
            "follow",
            "--after",
            &cursor.to_string(),
            "--count",
            "2",
        ],
    );
    let output = finish(child).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let events = json_lines(&output);
    assert_eq!(events.len(), 2, "{}", stdout(&output));
    let cursors = events
        .iter()
        .map(|event| event["cursor"].as_u64().expect("cursor"))
        .collect::<Vec<_>>();
    assert_eq!(cursors, [cursor + 1, cursor + 2]);
    assert!(EVACUATION.contains(&events[0]["operation"].as_str().expect("operation")));
}

#[tokio::test]
async fn events_follow_since_replays_the_window_and_then_follows() {
    let daemon = Daemon::start().await;
    evacuate(&daemon);

    let child = spawn(
        &daemon,
        &["events", "follow", "--since", "10m", "--count", "1"],
    );
    let output = finish(child).await;
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).starts_with("cursor=1 "),
        "{}",
        stdout(&output)
    );
}

#[tokio::test]
async fn events_follow_without_a_cursor_waits_for_new_events() {
    let daemon = Daemon::start().await;
    let child = spawn(
        &daemon,
        &["--output", "json", "events", "follow", "--count", "1"],
    );
    let watcher = spawn_blocking(move || child.wait_with_output());
    tokio::pin!(watcher);
    let output = loop {
        evacuate(&daemon);
        if let Ok(joined) = timeout(Duration::from_millis(50), &mut watcher).await {
            break joined.expect("join").expect("output");
        }
    };
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        EVACUATION.contains(
            &json_lines(&output)[0]["operation"]
                .as_str()
                .expect("operation")
        )
    );
}

#[tokio::test]
async fn ctrl_c_ends_events_follow_with_status_130_and_no_noise() {
    let daemon = Daemon::start().await;
    evacuate(&daemon);
    let mut child = spawn(&daemon, &["events", "follow", "--after", "0"]);
    let mut lines = BufReader::new(child.stdout.take().expect("stdout"));
    let mut first = String::new();
    lines.read_line(&mut first).expect("the first event");
    assert!(!first.is_empty());

    let signalled = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill");
    assert!(signalled.success());
    let status = spawn_blocking(move || child.wait_with_output())
        .await
        .expect("join")
        .expect("output");
    assert_eq!(status.status.code(), Some(130));
    assert!(stderr(&status).is_empty(), "{}", stderr(&status));
}

#[tokio::test]
async fn audit_lists_what_the_operator_did_with_filters_and_a_window() {
    let daemon = Daemon::start().await;
    evacuate(&daemon);

    let all = daemon.run(&["--output", "json", "audit", "list"]);
    assert!(all.status.success(), "{}", stderr(&all));
    let page = serde_json::from_slice::<Value>(&all.stdout).expect("JSON");
    assert!(
        page["records"]
            .as_array()
            .expect("records")
            .iter()
            .any(|record| record["operation"] == "server_evacuation"),
        "{page}"
    );

    let control = daemon.run(&[
        "audit",
        "list",
        "--category",
        "control",
        "--result",
        "succeeded",
    ]);
    assert!(
        stdout(&control).contains("server_evacuation"),
        "{}",
        stdout(&control)
    );
    assert!(
        stdout(&control).contains("OPERATION"),
        "{}",
        stdout(&control)
    );

    let window = daemon.run(&["audit", "list", "--since", "1h"]);
    assert!(
        stdout(&window).contains("server_evacuation"),
        "{}",
        stdout(&window)
    );

    let none = daemon.run(&["audit", "list", "--category", "import"]);
    assert!(none.status.success());
    assert!(stderr(&none).contains("no records"), "{}", stderr(&none));

    let status = daemon.run(&["--output", "json", "audit", "status"]);
    let status = serde_json::from_slice::<Value>(&status.stdout).expect("JSON");
    assert!(status["audit"]["recordCount"].as_u64().expect("count") >= 1);
    let table = daemon.run(&["audit", "status"]);
    assert!(stdout(&table).contains("persistent"), "{}", stdout(&table));
}

#[tokio::test]
async fn audit_tail_prints_new_records_and_stops_at_the_count() {
    let daemon = Daemon::start().await;
    let child = spawn(
        &daemon,
        &[
            "--output",
            "json",
            "audit",
            "tail",
            "--interval",
            "100ms",
            "--count",
            "1",
        ],
    );
    let watcher = spawn_blocking(move || child.wait_with_output());
    tokio::pin!(watcher);
    let output = loop {
        evacuate(&daemon);
        if let Ok(joined) = timeout(Duration::from_millis(50), &mut watcher).await {
            break joined.expect("join").expect("output");
        }
    };
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        EVACUATION.contains(
            &json_lines(&output)[0]["operation"]
                .as_str()
                .expect("operation")
        )
    );

    let replay = spawn(
        &daemon,
        &[
            "--output", "json", "audit", "tail", "--after", "0", "--count", "1",
        ],
    );
    let replayed = finish(replay).await;
    assert_eq!(json_lines(&replayed).len(), 1);
}

#[tokio::test]
async fn events_follow_reconnects_from_the_last_event_it_printed() {
    let directory = TempDir::new().expect("directory");
    let socket = directory.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let server = std::thread::spawn(move || {
        let mut requested = Vec::new();
        for (cursor, last) in [(6, false), (7, true)] {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut lines = BufReader::new(stream.try_clone().expect("clone")).lines();
            requested.push(lines.next().expect("request line").expect("text"));
            for line in lines.by_ref() {
                if line.expect("header").is_empty() {
                    break;
                }
            }
            let event = format!(
                "id: {cursor}\nevent: configuration_reload\ndata: {{\"cursor\":{cursor},\"event\":\"configuration_reload\"}}\n\n"
            );
            let frames = format!("event: ready\ndata: {{\"cursor\":5}}\n\n{event}");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{frames}\r\n",
                frames.len()
            )
            .expect("frames");
            if last {
                stream.write_all(b"0\r\n\r\n").expect("end");
            }
        }
        requested
    });

    let output = spawn_blocking(move || {
        cli_command()
            .args([
                "--socket",
                socket.to_str().expect("path"),
                "--output",
                "json",
            ])
            .args(["events", "follow", "--after", "5", "--count", "2"])
            .output()
            .expect("oxiroute CLI")
    })
    .await
    .expect("join");
    assert!(output.status.success(), "{}", stderr(&output));
    let cursors = json_lines(&output)
        .iter()
        .map(|event| event["cursor"].as_u64().expect("cursor"))
        .collect::<Vec<_>>();
    assert_eq!(cursors, [6, 7]);
    let requested = server.join().expect("server");
    assert_eq!(requested[0], "GET /api/v2/events/stream?after=5 HTTP/1.1");
    assert_eq!(requested[1], "GET /api/v2/events/stream?after=6 HTTP/1.1");
}
