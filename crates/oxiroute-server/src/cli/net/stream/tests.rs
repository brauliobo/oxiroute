use std::{
    io::{BufRead as _, BufReader, Read as _, Write as _},
    os::unix::net::{UnixListener, UnixStream},
    sync::mpsc,
    thread,
    time::Duration,
};

use tempfile::TempDir;

use crate::cli::{
    error::CliError,
    net::{Client, Endpoint, Timeouts},
};

const SILENCE: Duration = Duration::from_secs(5);

fn serve(
    directory: &TempDir,
    answer: impl FnOnce(&mut UnixStream) + Send + 'static,
) -> (Client, thread::JoinHandle<()>) {
    let path = directory.path().join("daemon.sock");
    let listener = UnixListener::bind(&path).expect("bind");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut request = BufReader::new(stream.try_clone().expect("clone"));
        let mut line = String::new();
        while request.read_line(&mut line).expect("request") > 2 {
            line.clear();
        }
        answer(&mut stream);
    });
    let endpoint = Endpoint::unix(path.to_str().expect("path")).expect("endpoint");
    (Client::new(endpoint, None, Timeouts::DEFAULT), server)
}

#[test]
fn chunks_are_decoded_as_they_arrive() {
    let directory = TempDir::new().expect("directory");
    let (proceed, wait) = mpsc::channel::<()>();
    let (client, server) = serve(&directory, move |stream| {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\nfirst\n\r\n")
            .expect("first chunk");
        wait.recv().expect("the client read the first line");
        stream
            .write_all(b"7\r\nsecond\n\r\n0\r\n\r\n")
            .expect("second chunk");
    });
    let mut lines = client
        .stream("/events", "text/event-stream", &[], SILENCE)
        .expect("stream")
        .lines();
    assert_eq!(lines.next().expect("line").expect("text"), "first");
    proceed.send(()).expect("signal");
    assert_eq!(lines.next().expect("line").expect("text"), "second");
    assert!(lines.next().is_none());
    server.join().expect("server");
}

#[test]
fn a_length_framed_body_ends_at_its_length() {
    let directory = TempDir::new().expect("directory");
    let (client, server) = serve(&directory, |stream| {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello-extra")
            .expect("answer");
    });
    let mut body = String::new();
    client
        .stream("/file", "application/octet-stream", &[], SILENCE)
        .expect("stream")
        .read_to_string(&mut body)
        .expect("body");
    assert_eq!(body, "hello");
    server.join().expect("server");
}

#[test]
fn a_failure_status_is_an_error_with_the_code_of_its_body() {
    let directory = TempDir::new().expect("directory");
    let (client, server) = serve(&directory, |stream| {
        let body =
            r#"{"error":{"code":"recording_not_found","message":"the recording does not exist"}}"#;
        write!(
            stream,
            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .expect("answer");
    });
    let Err(error) = client.stream("/file", "application/octet-stream", &[], SILENCE) else {
        panic!("a 404 is not a stream");
    };
    assert!(
        matches!(&error, CliError::Status(404, text) if text.starts_with("recording_not_found")),
        "{error}"
    );
    server.join().expect("server");
}

#[test]
fn a_silent_server_ends_the_read_with_a_timeout() {
    let directory = TempDir::new().expect("directory");
    let (release, held) = mpsc::channel::<()>();
    let (client, server) = serve(&directory, move |stream| {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .expect("head");
        held.recv().expect("the client gave up");
    });
    let mut body = client
        .stream(
            "/events",
            "text/event-stream",
            &[],
            Duration::from_millis(100),
        )
        .expect("stream");
    let error = body.read(&mut [0; 8]).expect_err("silence");
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
    release.send(()).expect("release");
    server.join().expect("server");
}
