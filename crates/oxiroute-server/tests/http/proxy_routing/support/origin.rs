use crate::http_support::read_request_head as read_request_head_bytes;
use crate::support::wire::read_request_head;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio::time::timeout;

const NO_ORIGIN_CONTACT_WINDOW: Duration = Duration::from_millis(100);

pub(crate) async fn capturing_origin(
    response_head: &'static str,
) -> (SocketAddr, oneshot::Receiver<String>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("capturing origin bind");
    let address = listener.local_addr().expect("capturing origin address");
    let (seen_tx, seen_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("capturing origin accept");
        let request = read_request_head_bytes(&mut stream)
            .await
            .expect("capturing origin request");
        let _ = seen_tx.send(String::from_utf8(request).expect("request UTF-8"));
        let response = format!(
            "HTTP/1.1 200 OK\r\n{response_head}Content-Length: 2\r\nConnection: close\r\n\r\nok"
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("capturing origin response");
    });
    (address, seen_rx, task)
}

/// An origin that answers every request with `status`, naming itself in the body and `X-Origin`.
pub(crate) struct StatusOrigin {
    pub(crate) address: SocketAddr,
    pub(crate) bodies: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    pub(crate) heads: Arc<std::sync::Mutex<Vec<String>>>,
    pub(crate) task: JoinHandle<()>,
}

impl StatusOrigin {
    pub(crate) async fn start(status: u16, name: &'static str) -> Self {
        Self::start_delayed(status, name, Duration::ZERO).await
    }

    pub(crate) async fn start_delayed(status: u16, name: &'static str, delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = Arc::clone(&bodies);
        let heads = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded_heads = Arc::clone(&heads);
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                let mut request = read_request_head_bytes(&mut stream)
                    .await
                    .expect("origin request");
                let head_end = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .expect("request head")
                    + 4;
                let head = String::from_utf8_lossy(&request[..head_end]).to_ascii_lowercase();
                recorded_heads
                    .lock()
                    .expect("recorded heads")
                    .push(head.clone());
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .map_or(0, |value| value.trim().parse::<usize>().expect("length"));
                let mut chunk = [0_u8; 1024];
                while request.len() < head_end + length {
                    let read = stream.read(&mut chunk).await.expect("origin body");
                    request.extend_from_slice(&chunk[..read]);
                }
                recorded
                    .lock()
                    .expect("recorded bodies")
                    .push(request[head_end..].to_vec());
                tokio::time::sleep(delay).await;
                let response = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nX-Origin: {name}\r\nConnection: close\r\n\r\n{name}",
                    name.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        Self {
            address,
            bodies,
            heads,
            task,
        }
    }

    pub(crate) fn heads(&self) -> Vec<String> {
        self.heads.lock().expect("recorded heads").clone()
    }

    pub(crate) fn hits(&self) -> usize {
        self.bodies.lock().expect("recorded bodies").len()
    }
}

impl Drop for StatusOrigin {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) struct Origin {
    pub(crate) address: SocketAddr,
    pub(crate) accepted: Arc<AtomicUsize>,
    pub(crate) task: Option<JoinHandle<()>>,
}

impl Origin {
    pub(crate) async fn start(name: &'static str, expected_requests: usize) -> Self {
        Self::start_with_connection_header(name, expected_requests, true).await
    }

    pub(crate) async fn start_without_connection_header(
        name: &'static str,
        expected_requests: usize,
    ) -> Self {
        Self::start_with_connection_header(name, expected_requests, false).await
    }

    pub(crate) async fn start_with_cache_control(
        name: &'static str,
        expected_requests: usize,
        cache_control: Option<&'static str>,
    ) -> Self {
        Self::start_with_options(name, expected_requests, cache_control, true).await
    }

    async fn start_with_connection_header(
        name: &'static str,
        expected_requests: usize,
        connection_header: bool,
    ) -> Self {
        Self::start_with_options(name, expected_requests, None, connection_header).await
    }

    async fn start_with_options(
        name: &'static str,
        expected_requests: usize,
        cache_control: Option<&'static str>,
        connection_header: bool,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_task = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                accepted_by_task.fetch_add(1, Ordering::SeqCst);
                read_request_head(&mut stream)
                    .await
                    .expect("origin request");
                let cache_header = cache_control
                    .map(|value| format!("Cache-Control: {value}\r\n"))
                    .unwrap_or_default();
                let connection = if connection_header {
                    "Connection: close\r\n"
                } else {
                    ""
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n{cache_header}{connection}\r\n{name}",
                    name.len(),
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("origin response");
                stream.shutdown().await.expect("origin shutdown");
            }
        });

        Self {
            address,
            accepted,
            task: Some(task),
        }
    }

    pub(crate) async fn start_with_surrogate_tag(
        name: &'static str,
        expected_requests: usize,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_task = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                accepted_by_task.fetch_add(1, Ordering::SeqCst);
                read_request_head(&mut stream)
                    .await
                    .expect("origin request");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nCache-Control: max-age=60\r\nSurrogate-Key: page\r\nConnection: close\r\n\r\n{name}",
                    name.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("origin response");
                stream.shutdown().await.expect("origin shutdown");
            }
        });

        Self {
            address,
            accepted,
            task: Some(task),
        }
    }

    pub(crate) async fn start_with_set_cookie(
        name: &'static str,
        expected_requests: usize,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_task = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                accepted_by_task.fetch_add(1, Ordering::SeqCst);
                read_request_head(&mut stream)
                    .await
                    .expect("origin request");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nCache-Control: max-age=60\r\nSet-Cookie: session=secret\r\nConnection: close\r\n\r\n{name}",
                    name.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("origin response");
                stream.shutdown().await.expect("origin shutdown");
            }
        });

        Self {
            address,
            accepted,
            task: Some(task),
        }
    }

    pub(crate) async fn start_fail_once(name: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_task = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                accepted_by_task.fetch_add(1, Ordering::SeqCst);
                read_request_head(&mut stream)
                    .await
                    .expect("origin request");
                if attempt == 0 {
                    continue;
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{name}",
                    name.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("origin response");
                stream.shutdown().await.expect("origin shutdown");
            }
        });

        Self {
            address,
            accepted,
            task: Some(task),
        }
    }

    pub(crate) async fn start_chunked(name: &'static str, expected_requests: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_task = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                accepted_by_task.fetch_add(1, Ordering::SeqCst);
                read_request_head(&mut stream)
                    .await
                    .expect("origin request");
                let first = &name.as_bytes()[..name.len().min(6)];
                let second = &name.as_bytes()[name.len().min(6)..];
                let response = format!(
                    "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nCache-Control: max-age=60\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n{:x}\r\n{}\r\n0\r\n\r\n",
                    first.len(),
                    String::from_utf8_lossy(first),
                    second.len(),
                    String::from_utf8_lossy(second),
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("origin response");
                stream.shutdown().await.expect("origin shutdown");
            }
        });

        Self {
            address,
            accepted,
            task: Some(task),
        }
    }

    pub(crate) async fn start_incomplete(expected_requests: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_task = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                accepted_by_task.fetch_add(1, Ordering::SeqCst);
                read_request_head(&mut stream)
                    .await
                    .expect("origin request");
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nCache-Control: max-age=60\r\nConnection: close\r\n\r\nshort",
                    )
                    .await
                    .expect("incomplete origin response");
            }
        });
        Self {
            address,
            accepted,
            task: Some(task),
        }
    }

    pub(crate) async fn start_stale_then_failure(name: &'static str, server_error: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_task = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                accepted_by_task.fetch_add(1, Ordering::SeqCst);
                read_request_head(&mut stream)
                    .await
                    .expect("origin request");
                if attempt == 0 {
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nCache-Control: max-age=0\r\nAge: 1\r\nConnection: close\r\n\r\n{name}",
                        name.len()
                    );
                    stream
                        .write_all(response.as_bytes())
                        .await
                        .expect("cache seed response");
                } else if server_error {
                    stream
                        .write_all(
                            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await
                        .expect("server error response");
                }
            }
        });
        Self {
            address,
            accepted,
            task: Some(task),
        }
    }

    pub(crate) async fn start_status(status: u16, body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_task = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("origin accept");
            accepted_by_task.fetch_add(1, Ordering::SeqCst);
            read_request_head(&mut stream)
                .await
                .expect("origin request");
            let response = format!(
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("origin response");
            stream.shutdown().await.expect("origin shutdown");
        });

        Self {
            address,
            accepted,
            task: Some(task),
        }
    }

    pub(crate) async fn start_silent() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_task = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("origin accept");
            accepted_by_task.fetch_add(1, Ordering::SeqCst);
            let mut request = Vec::new();
            stream
                .read_to_end(&mut request)
                .await
                .expect("origin request close");
        });

        Self {
            address,
            accepted,
            task: Some(task),
        }
    }

    pub(crate) async fn start_disconnecting() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_task = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("origin accept");
            accepted_by_task.fetch_add(1, Ordering::SeqCst);
            read_request_head(&mut stream)
                .await
                .expect("origin request");
        });

        Self {
            address,
            accepted,
            task: Some(task),
        }
    }

    pub(crate) async fn assert_not_contacted(mut self) {
        let contacted = timeout(NO_ORIGIN_CONTACT_WINDOW, async {
            while self.accepted.load(Ordering::SeqCst) == 0 {
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .is_ok();
        assert!(!contacted, "origin unexpectedly accepted a connection");
        self.task.take().expect("origin task").abort();
    }

    pub(crate) async fn finish(mut self) {
        let task = self.task.take().expect("origin task");
        task.await.expect("origin task completed");
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

pub(crate) struct RevalidatingOrigin {
    pub(crate) address: SocketAddr,
    pub(crate) accepted: Arc<AtomicUsize>,
    pub(crate) conditional_requests: Arc<AtomicUsize>,
    pub(crate) task: Option<JoinHandle<()>>,
}

impl RevalidatingOrigin {
    pub(crate) async fn start(name: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_task = Arc::clone(&accepted);
        let conditional_requests = Arc::new(AtomicUsize::new(0));
        let conditional_by_task = Arc::clone(&conditional_requests);
        let task = tokio::spawn(async move {
            for request_number in 0..2 {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                accepted_by_task.fetch_add(1, Ordering::SeqCst);
                let request = read_request_head_bytes(&mut stream)
                    .await
                    .expect("origin request");
                let conditional = request
                    .windows(b"If-None-Match: \"v1\"".len())
                    .any(|window| window.eq_ignore_ascii_case(b"If-None-Match: \"v1\""));
                if conditional {
                    conditional_by_task.fetch_add(1, Ordering::SeqCst);
                }
                let response = if request_number == 1 && conditional {
                    "HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\nCache-Control: max-age=0\r\nConnection: close\r\n\r\n".to_owned()
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"v1\"\r\nCache-Control: max-age=0\r\nConnection: close\r\n\r\n{name}",
                        name.len()
                    )
                };
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("origin response");
                stream.shutdown().await.expect("origin shutdown");
            }
        });

        Self {
            address,
            accepted,
            conditional_requests,
            task: Some(task),
        }
    }

    pub(crate) async fn finish(mut self) {
        self.task
            .take()
            .expect("origin task")
            .await
            .expect("origin task completed");
    }
}

impl Drop for RevalidatingOrigin {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
