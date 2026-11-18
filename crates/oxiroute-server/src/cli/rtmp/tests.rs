use clap::Parser as _;
use serde_json::json;

use super::{
    RtmpCommand,
    recording::{Status, encode, list_path},
};
use crate::cli::{Cli, Command, view::View};

fn parse(words: &[&str]) -> Result<RtmpCommand, clap::Error> {
    let mut all = vec!["oxiroute", "rtmp"];
    all.extend_from_slice(words);
    Cli::try_parse_from(all).map(|cli| match cli.command {
        Command::Rtmp { command } => command,
        _ => panic!("rtmp command"),
    })
}

#[test]
fn recording_ids_are_encoded_into_the_path() {
    assert_eq!(encode("abc-1.2_x~"), "abc-1.2_x~");
    assert_eq!(encode("a b/c?d"), "a%20b%2Fc%3Fd");
}

#[test]
fn a_recording_query_names_only_the_filters_that_were_given() {
    assert_eq!(
        list_path(None, None, None, 100, None),
        "/api/v1/rtmp/recordings?limit=100"
    );
    assert_eq!(
        list_path(
            Some("front door"),
            Some("archive"),
            Some(Status::Partial),
            5,
            Some("17.id")
        ),
        "/api/v1/rtmp/recordings?limit=5&stream=front%20door&recorder=archive&status=partial&cursor=17.id"
    );
}

#[test]
fn destructive_recording_commands_need_confirmation() {
    assert!(parse(&["recording", "delete", "id"]).is_err());
    assert!(parse(&["recording", "delete", "id", "--yes"]).is_ok());
    assert!(parse(&["recording", "download", "id"]).is_ok());
    assert!(parse(&["recording", "list", "--limit", "0"]).is_err());
    assert!(parse(&["recording", "list", "--status", "finished"]).is_err());
    assert!(parse(&["publisher", "kick"]).is_err());
}

#[test]
fn sessions_show_their_role_stream_and_age() {
    let value = json!({ "asOfUnixMs": 70_000, "clients": [{
        "id": "c-1", "service": "edge", "application": "live", "stream": "cam", "role": "publisher",
        "peerIp": "192.0.2.7:5000", "connectedAtUnixMs": 10_000, "revision": "4",
    }] });
    assert_eq!(
        View::RtmpClients.document(&value).plain(),
        "id=c-1 service=edge application=live stream=cam role=publisher peer=192.0.2.7:5000 age_ms=60000 revision=4\n"
    );
}

#[test]
fn the_quota_shows_the_share_used_and_what_retention_evicted() {
    let value = json!({ "roots": [{
        "recorders": [{ "service": "edge", "application": "live", "recorder": "archive" }],
        "bytes_used": "1048576", "max_bytes": "4194304", "files": 3, "max_files": 10,
        "retention": "delete_oldest", "retention_max_age_ms": 86_400_000,
        "evicted_files": "2", "evicted_bytes": "2048",
    }] });
    let plain = View::RtmpQuota.document(&value).plain();
    assert!(plain.contains("recorders=archive"), "{plain}");
    assert!(plain.contains("share_pct=25.0"), "{plain}");
    assert!(plain.contains("retention=delete_oldest"), "{plain}");
    assert!(plain.contains("max_age_ms=86400000"), "{plain}");
    assert!(plain.contains("evicted_files=2"), "{plain}");
}

#[test]
fn a_full_page_of_recordings_points_at_the_next_cursor() {
    let value = json!({ "recordings": [{
        "id": "r-1", "name": "cam-1.flv", "stream": "cam", "status": "complete",
        "size_bytes": "2048", "duration_ms": 4_000, "started_at_unix_ms": 1_700_000_000_000_u64,
        "recorders": [{ "recorder": "archive" }],
    }], "next_cursor": "17.r-1" });
    let text = View::RtmpRecordings.document(&value).table();
    assert!(text.stdout.contains("cam-1.flv"), "{}", text.stdout);
    assert_eq!(
        text.notices,
        ["more recordings are available; continue with --cursor 17.r-1"]
    );
}
