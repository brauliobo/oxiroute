use std::io;

use super::sse::{Frame, frames};

fn read(text: &str) -> Vec<Frame> {
    frames(text.lines().map(|line| Ok(line.to_owned())))
        .collect::<io::Result<Vec<_>>>()
        .expect("frames")
}

#[test]
fn an_event_has_an_id_a_name_and_data() {
    let parsed = read("id: 7\nevent: generation_activated\ndata: {\"cursor\":7}\n\n");
    assert_eq!(
        parsed,
        [Frame {
            id: Some(7),
            name: "generation_activated".into(),
            data: "{\"cursor\":7}".into(),
        }]
    );
}

#[test]
fn heartbeat_comments_and_unknown_fields_are_skipped() {
    let parsed = read(": heartbeat\n\nretry: 5000\n\nevent: ready\ndata: {\"cursor\":3}\n\n");
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[1].name, "ready");
}

#[test]
fn data_lines_join_and_a_frame_ends_at_its_blank_line() {
    let parsed = read("data: a\ndata: b\n\ndata: c\n\ndata: unfinished\n");
    assert_eq!(
        parsed
            .iter()
            .map(|frame| frame.data.as_str())
            .collect::<Vec<_>>(),
        ["a\nb", "c"]
    );
}

#[test]
fn a_read_error_ends_the_frames_with_that_error() {
    let lines = [
        Ok("event: ready".to_owned()),
        Err(io::Error::from(io::ErrorKind::TimedOut)),
    ];
    let mut parsed = frames(lines.into_iter());
    assert_eq!(
        parsed
            .next()
            .expect("an item")
            .expect_err("an error")
            .kind(),
        io::ErrorKind::TimedOut
    );
}
