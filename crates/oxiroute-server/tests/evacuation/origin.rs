use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Answers as soon as the request arrived.
    Reply,
    /// Reads the request, then waits for the test to release it.
    Hold,
    /// Sends the response header and the first bytes at once, the rest when released.
    Stream,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Event {
    Arrived {
        request_id: String,
        body: String,
    },
    /// The proxy closed the connection before the origin answered.
    Dropped {
        request_id: String,
    },
}

/// A localhost origin that stays bound for the whole test and reports what it sees.
pub struct Origin {
    pub address: SocketAddr,
    pub events: mpsc::UnboundedReceiver<Event>,
    pub gate: Arc<Semaphore>,
    pub task: JoinHandle<()>,
}

impl Origin {
    pub async fn start(name: String, mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let (events_tx, events) = mpsc::unbounded_channel();
        let gate = Arc::new(Semaphore::new(0));
        let served = Arc::clone(&gate);
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.expect("origin accept");
                let connection = Connection {
                    name: name.clone(),
                    mode,
                    gate: Arc::clone(&served),
                    events: events_tx.clone(),
                };
                tokio::spawn(connection.serve(stream));
            }
        });
        Self {
            address,
            events,
            gate,
            task,
        }
    }

    /// Lets `count` held requests answer.
    pub fn release(&self, count: usize) {
        self.gate.add_permits(count);
    }

    pub async fn next(&mut self) -> Event {
        timeout(TEST_TIMEOUT, self.events.recv())
            .await
            .expect("origin event in time")
            .expect("origin task alive")
    }

    pub async fn arrival(&mut self, request_id: &str) -> String {
        match self.next().await {
            Event::Arrived {
                request_id: id,
                body,
            } if id == request_id => body,
            other => panic!("expected {request_id} to arrive, got {other:?}"),
        }
    }

    pub async fn dropped(&mut self, request_id: &str) {
        assert_eq!(
            self.next().await,
            Event::Dropped {
                request_id: request_id.into()
            }
        );
    }

    /// Everything the origin reported meanwhile.
    pub fn seen(&mut self) -> Vec<Event> {
        std::iter::from_fn(|| self.events.try_recv().ok()).collect()
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct Connection {
    pub name: String,
    pub mode: Mode,
    pub gate: Arc<Semaphore>,
    pub events: mpsc::UnboundedSender<Event>,
}

impl Connection {
    async fn serve(self, mut stream: TcpStream) {
        while let Some((request_id, body)) = read_request(&mut stream).await {
            self.events
                .send(Event::Arrived {
                    request_id: request_id.clone(),
                    body,
                })
                .expect("test alive");
            let proceeded = match self.mode {
                Mode::Reply => true,
                Mode::Hold => self.released(&mut stream, &request_id).await,
                Mode::Stream => self.stream(&mut stream, &request_id).await,
            };
            if !proceeded {
                return;
            }
            if self.mode != Mode::Stream {
                let body = format!("from-{}", self.name);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-Origin: {}\r\n\r\n",
                    body.len(),
                    self.name
                );
                if stream
                    .write_all(format!("{head}{body}").as_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    }

    async fn stream(&self, stream: &mut TcpStream, request_id: &str) -> bool {
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nX-Origin: {}\r\n\r\nhello",
            self.name
        );
        if stream.write_all(head.as_bytes()).await.is_err()
            || !self.released(stream, request_id).await
        {
            return false;
        }
        stream.write_all(b"world").await.is_ok()
    }

    /// Waits for the test to release the request; false when the proxy closed the connection first.
    async fn released(&self, stream: &mut TcpStream, request_id: &str) -> bool {
        let mut byte = [0; 1];
        tokio::select! {
            permit = self.gate.acquire() => {
                permit.expect("gate open").forget();
                true
            }
            _ = stream.read(&mut byte) => {
                self.events.send(Event::Dropped { request_id: request_id.into() }).expect("test alive");
                false
            }
        }
    }
}

/// The request id and body of the next request on the connection.
pub async fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
    let mut received = Vec::new();
    let mut buffer = [0; 1024];
    let head_end = loop {
        if let Some(end) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        received.extend_from_slice(&buffer[..read]);
    };
    let head = String::from_utf8_lossy(&received[..head_end]).to_ascii_lowercase();
    let header = |name: &str| {
        head.lines()
            .find_map(|line| line.strip_prefix(name))
            .map(|value| value.trim().to_owned())
    };
    let length =
        header("content-length:").map_or(0, |value| value.parse::<usize>().expect("length"));
    while received.len() < head_end + length {
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        received.extend_from_slice(&buffer[..read]);
    }
    let body = String::from_utf8_lossy(&received[head_end..head_end + length]).into_owned();
    Some((header("x-request-id:").unwrap_or_default(), body))
}
