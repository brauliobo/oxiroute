use std::{fs, net::TcpListener, thread};

use tempfile::tempdir;

use super::*;

#[test]
fn ranges_are_single_contiguous_and_bounded() {
    assert_eq!(
        VodRange::parse(Some("bytes=2-5"), 10),
        Ok(Some(VodRange { start: 2, end: 5 }))
    );
    assert_eq!(
        VodRange::parse(Some("bytes=-3"), 10),
        Ok(Some(VodRange { start: 7, end: 9 }))
    );
    assert_eq!(
        VodRange::parse(Some("bytes=9-"), 10),
        Ok(Some(VodRange { start: 9, end: 9 }))
    );
    assert!(matches!(
        VodRange::parse(Some("bytes=1-2,4-5"), 10),
        Err(VodError::InvalidRange)
    ));
}

#[test]
fn local_source_rejects_traversal_and_symlinked_files() {
    let root = tempdir().expect("root");
    fs::write(root.path().join("movie.flv"), b"movie").expect("movie");
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.path().join("movie.flv"), root.path().join("link.flv"))
        .expect("symlink");
    let application = VodApplication::new(
        "live",
        "broadcast",
        VodLimits {
            max_sessions: 1,
            max_file_bytes: 1024,
            max_duration: Duration::from_mins(1),
        },
        [VodSourceDefinition::Local {
            name: "archive".into(),
            root_directory: root.path().to_path_buf(),
        }],
        &RtmpOutboundPolicy {
            deny_private: false,
            ..RtmpOutboundPolicy::default()
        },
    )
    .expect("VOD application");
    assert!(matches!(
        application.validate_request("missing", "movie.flv"),
        Err(VodError::SourceNotFound)
    ));
    assert!(matches!(
        application.validate_request("archive", "../movie.flv"),
        Err(VodError::InvalidPath)
    ));
    assert!(matches!(
        application.open("archive", "../movie.flv"),
        Err(VodError::InvalidPath)
    ));
    #[cfg(unix)]
    assert!(matches!(
        application.open("archive", "link.flv"),
        Err(VodError::Fetch)
    ));
    let object = application
        .open("archive", "movie.flv")
        .expect("movie object");
    assert_eq!(object.bytes(), b"movie");
}

#[test]
fn local_source_enforces_session_limit() {
    let root = tempdir().expect("root");
    fs::write(root.path().join("movie.flv"), b"movie").expect("movie");
    let application = VodApplication::new(
        "live",
        "broadcast",
        VodLimits {
            max_sessions: 1,
            max_file_bytes: 1024,
            max_duration: Duration::from_mins(1),
        },
        [VodSourceDefinition::Local {
            name: "archive".into(),
            root_directory: root.path().to_path_buf(),
        }],
        &RtmpOutboundPolicy {
            deny_private: false,
            ..RtmpOutboundPolicy::default()
        },
    )
    .expect("VOD application");
    let first = application
        .open("archive", "movie.flv")
        .expect("first object");
    assert!(matches!(
        application.open("archive", "movie.flv"),
        Err(VodError::SessionLimit)
    ));
    drop(first);
    assert!(application.open("archive", "movie.flv").is_ok());
}

#[test]
fn http_source_rejects_chunked_responses_and_respects_body_bound() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP listener");
    let address = listener.local_addr().expect("HTTP address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("HTTP request");
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).expect("HTTP read");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .expect("HTTP response");
    });
    let origin = format!("http://{address}");
    let result = HttpOrigin::parse(
        &origin,
        &RtmpOutboundPolicy {
            deny_private: false,
            ..RtmpOutboundPolicy::default()
        },
    )
    .expect("HTTP origin")
    .fetch("movie.flv", 1024);
    server.join().expect("HTTP server");
    assert!(matches!(result, Err(VodError::Fetch)));
}

#[test]
fn http_source_rejects_ambiguous_content_length_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP listener");
    let address = listener.local_addr().expect("HTTP address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("HTTP request");
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).expect("HTTP read");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nContent-Length: 3\r\n\r\nvod")
            .expect("HTTP response");
    });
    let origin = format!("http://{address}");
    let result = HttpOrigin::parse(
        &origin,
        &RtmpOutboundPolicy {
            deny_private: false,
            ..RtmpOutboundPolicy::default()
        },
    )
    .expect("HTTP origin")
    .fetch("movie.flv", 1024);
    server.join().expect("HTTP server");
    assert!(matches!(result, Err(VodError::Fetch)));
}
