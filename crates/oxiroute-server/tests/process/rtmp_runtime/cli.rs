//! `oxiroute rtmp` against a real daemon with live publishers, viewers and stored recordings.

use std::process::{Command, Output};

use super::{inventory::*, *};

struct Operator {
    management: SocketAddr,
    token: std::path::PathBuf,
}

impl Operator {
    fn new(server: &ServerProcess, management: SocketAddr) -> Self {
        Self {
            management,
            token: server.token_path.clone().expect("token file"),
        }
    }

    fn run(&self, words: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_oxiroute"))
            .env_remove("OXIROUTE_ENDPOINT")
            .env_remove("OXIROUTE_MANAGEMENT_TOKEN_FILE")
            .arg("--endpoint")
            .arg(format!("http://{}", self.management))
            .arg("--token-file")
            .arg(&self.token)
            .args(words)
            .output()
            .expect("oxiroute CLI")
    }

    fn json(&self, words: &[&str]) -> Value {
        let output = self.run(&[&["--output", "json"], words].concat());
        assert!(output.status.success(), "{words:?}: {}", stderr(&output));
        serde_json::from_slice(&output.stdout).expect("JSON")
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[tokio::test]
async fn streams_publishers_and_viewers_are_listed_and_kicked() {
    let directory = TempDir::new().expect("recording directory");
    let root = create_secure_root(directory.path(), "cli-recordings");
    let unused = create_secure_root(directory.path(), "cli-unused");
    let management = reserve_tcp_address();
    let rtmp = reserve_tcp_address();
    let config = inventory_config(management, rtmp, &root, &unused);
    let mut server = ServerProcess::start(&config.validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management).await;
    server.wait_for_tcp(rtmp).await;
    let operator = Operator::new(&server, management);

    let mut publisher = RtmpWireClient::connect(rtmp, "continuous").await;
    publisher.publish("camera").await;
    publisher.publish_audio(0, &[0xaf, 0x00, 0x12]).await;
    publisher.publish_audio(20, &[0xaf, 0x01, 0x40]).await;
    let mut viewer = RtmpWireClient::connect(rtmp, "continuous").await;
    viewer.play("camera").await;
    wait_for_catalog(management, |catalog| {
        stream_for(catalog, "continuous").is_some_and(|stream| {
            stream["subscriber_count"] == 1 && stream["publisher"].is_object()
        })
    })
    .await;

    let streams = operator.json(&["rtmp", "stream", "list"]);
    let stream = streams["streams"][0].clone();
    assert_eq!(stream["name"], "camera");
    let stream_id = stream["id"].as_str().expect("stream id").to_owned();

    let publishers = operator.json(&["rtmp", "publisher", "list"]);
    assert_eq!(publishers["clients"].as_array().expect("clients").len(), 1);
    assert_eq!(publishers["clients"][0]["role"], "publisher");
    assert_eq!(publishers["clients"][0]["stream"], "camera");
    let viewers = operator.run(&["--output", "plain", "rtmp", "viewer", "list"]);
    assert!(viewers.status.success(), "{}", stderr(&viewers));
    let viewers = stdout(&viewers);
    assert_eq!(viewers.lines().count(), 1, "{viewers}");
    assert!(
        viewers.contains("role=subscriber stream=camera") || viewers.contains("role=subscriber"),
        "{viewers}"
    );
    let viewer_id = viewers
        .split_whitespace()
        .find_map(|pair| pair.strip_prefix("id="))
        .expect("the session id")
        .to_owned();

    let wrong_role = operator.run(&["rtmp", "publisher", "kick", &viewer_id]);
    assert_eq!(wrong_role.status.code(), Some(7), "{}", stderr(&wrong_role));
    let unknown = operator.run(&["rtmp", "viewer", "kick", "no-such-session"]);
    assert_eq!(unknown.status.code(), Some(6), "{}", stderr(&unknown));
    assert!(
        stderr(&unknown).contains("session_not_found"),
        "{}",
        stderr(&unknown)
    );

    let kicked = operator.json(&["rtmp", "viewer", "kick", &viewer_id]);
    assert_eq!(kicked["outcome"], "requested");
    assert_eq!(kicked["sessionId"], viewer_id.as_str());
    wait_for_catalog(management, |catalog| {
        stream_for(catalog, "continuous").is_some_and(|stream| {
            stream["subscriber_count"] == 0 && stream["publisher"].is_object()
        })
    })
    .await;

    let disconnected = operator.json(&["rtmp", "stream", "disconnect", &stream_id]);
    assert_eq!(disconnected["target"], "publisher");
    wait_for_catalog(management, |catalog| {
        stream_for(catalog, "continuous").is_none_or(|stream| stream["publisher"].is_null())
    })
    .await;
    let ended = operator.run(&["rtmp", "stream", "disconnect", &stream_id]);
    assert_eq!(ended.status.code(), Some(6), "{}", stderr(&ended));
    assert!(
        stderr(&ended).contains("stream_not_found"),
        "{}",
        stderr(&ended)
    );

    let relay = operator.run(&["rtmp", "relay", "reconnect", &stream_id, "mirror"]);
    assert_eq!(relay.status.code(), Some(9), "{}", stderr(&relay));
    drop((publisher, viewer));
    server.shutdown();
}

struct Recorded {
    directory: TempDir,
    root: std::path::PathBuf,
    server: ServerProcess,
    operator: Operator,
    recordings: Vec<Value>,
}

async fn recorded() -> Recorded {
    let directory = TempDir::new().expect("recording directory");
    let root = create_secure_root(directory.path(), "cli-recordings");
    let unused = create_secure_root(directory.path(), "cli-unused");
    let management = reserve_tcp_address();
    let rtmp = reserve_tcp_address();
    let config = inventory_config(management, rtmp, &root, &unused);
    let mut server = ServerProcess::start(&config.validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management).await;
    server.wait_for_tcp(rtmp).await;
    let operator = Operator::new(&server, management);
    publish_and_close(rtmp, "camera", 12).await;
    let listing = wait_for_recordings(management, "?stream=camera", |recordings| {
        recordings.len() >= 3 && recordings.iter().all(|entry| entry["status"] == "complete")
    })
    .await;
    let recordings = listing["recordings"]
        .as_array()
        .expect("recordings")
        .clone();
    Recorded {
        directory,
        root,
        server,
        operator,
        recordings,
    }
}

#[tokio::test]
async fn recordings_are_listed_filtered_paged_and_shown() {
    let Recorded {
        operator,
        recordings,
        server,
        directory: _directory,
        ..
    } = recorded().await;
    let id = recordings[0]["id"].as_str().expect("id");
    let name = recordings[0]["name"].as_str().expect("name");

    let listing = operator.json(&[
        "rtmp",
        "recording",
        "list",
        "--stream",
        "camera",
        "--status",
        "complete",
    ]);
    assert_eq!(
        listing["recordings"].as_array().expect("recordings").len(),
        recordings.len()
    );
    let table = operator.run(&["rtmp", "recording", "list", "--recorder", "archive"]);
    assert!(
        stdout(&table).contains(id) && stdout(&table).contains("complete"),
        "{}",
        stdout(&table)
    );
    let page = operator.run(&["rtmp", "recording", "list", "--limit", "1"]);
    assert!(
        stderr(&page).contains("continue with --cursor"),
        "{}",
        stderr(&page)
    );
    let none = operator.run(&["rtmp", "recording", "list", "--stream", "nothing"]);
    assert!(stderr(&none).contains("no recordings"), "{}", stderr(&none));

    assert_eq!(
        operator.json(&["rtmp", "recording", "show", id])["name"],
        name
    );
    let detail = operator.run(&["rtmp", "recording", "show", id]);
    assert!(
        stdout(&detail).contains("audio codec"),
        "{}",
        stdout(&detail)
    );
    server.shutdown();
}

#[tokio::test]
async fn the_quota_shows_each_root_with_its_retention() {
    let Recorded {
        operator,
        server,
        directory: _directory,
        ..
    } = recorded().await;

    let listing = operator.json(&["rtmp", "recording", "list", "--limit", "1"]);
    assert_eq!(listing["roots"][0]["retention"], "reject");
    let table = stdout(&operator.run(&["rtmp", "recording", "quota"]));
    assert!(
        table.contains("RETENTION") && table.contains("reject"),
        "{table}"
    );
    assert!(table.contains("archive"), "{table}");
    server.shutdown();
}

#[tokio::test]
async fn a_recording_is_downloaded_whole_and_never_replaced_without_force() {
    let Recorded {
        directory,
        root,
        operator,
        recordings,
        server,
    } = recorded().await;
    let id = recordings[0]["id"].as_str().expect("id");
    let on_disk =
        fs::read(root.join(recordings[0]["name"].as_str().expect("name"))).expect("recording file");
    let target = directory.path().join("downloaded.flv");
    let to = target.to_str().expect("path");
    let download =
        |extra: &[&str]| operator.run(&[&["rtmp", "recording", "download", id], extra].concat());

    let downloaded = download(&["--to", to]);
    assert!(downloaded.status.success(), "{}", stderr(&downloaded));
    assert_eq!(fs::read(&target).expect("downloaded file"), on_disk);
    assert!(
        stdout(&downloaded).contains("downloaded.flv"),
        "{}",
        stdout(&downloaded)
    );
    let again = download(&["--to", to]);
    assert_eq!(again.status.code(), Some(3), "{}", stderr(&again));
    assert!(download(&["--to", to, "--force"]).status.success());
    assert_eq!(download(&["--to", "-"]).stdout, on_disk);

    let missing = directory.path().join("missing.flv");
    let absent = operator.run(&[
        "rtmp",
        "recording",
        "download",
        "no-such-id",
        "--to",
        missing.to_str().expect("path"),
    ]);
    assert_eq!(absent.status.code(), Some(6), "{}", stderr(&absent));
    assert!(!missing.exists());
    let leftovers = fs::read_dir(directory.path())
        .expect("directory")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".partial"))
        .count();
    assert_eq!(leftovers, 0);
    server.shutdown();
}

#[tokio::test]
async fn a_recording_is_deleted_only_when_confirmed() {
    let Recorded {
        root,
        operator,
        recordings,
        server,
        directory: _directory,
    } = recorded().await;
    let id = recordings[0]["id"].as_str().expect("id");
    let name = recordings[0]["name"].as_str().expect("name");

    let unconfirmed = operator.run(&["rtmp", "recording", "delete", id]);
    assert_eq!(
        unconfirmed.status.code(),
        Some(2),
        "{}",
        stderr(&unconfirmed)
    );
    assert!(root.join(name).exists());
    let deleted = operator.json(&["rtmp", "recording", "delete", id, "--yes"]);
    assert_eq!(deleted["deleted"]["id"], id);
    let gone = operator.run(&["rtmp", "recording", "show", id]);
    assert_eq!(gone.status.code(), Some(6), "{}", stderr(&gone));
    assert!(!root.join(name).exists());
    let remaining = operator.json(&["rtmp", "recording", "list", "--stream", "camera"]);
    assert_eq!(
        remaining["recordings"]
            .as_array()
            .expect("recordings")
            .len(),
        recordings.len() - 1
    );
    server.shutdown();
}
