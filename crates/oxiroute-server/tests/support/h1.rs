pub struct RawHttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

pub async fn h1_request<S>(stream: &mut S, path: &str, close: bool) -> io::Result<RawHttpResponse>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let connection = if close { "close" } else { "keep-alive" };
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {PROXY_SERVER_NAME}\r\nConnection: {connection}\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;

    let mut response = read_until_headers(stream).await?;
    let header_end = find_header_end(&response).expect("complete response headers");
    let head = std::str::from_utf8(&response[..header_end])
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_ascii_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing HTTP status"))?;
    let content_length = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    let body_start = header_end + 4;
    let response_end = body_start + content_length;
    if response.len() < response_end {
        let received = response.len();
        response.resize(response_end, 0);
        stream.read_exact(&mut response[received..]).await?;
    } else {
        response.truncate(response_end);
    }
    Ok(RawHttpResponse {
        status,
        body: response[body_start..].to_vec(),
    })
}

async fn read_until_headers<S>(stream: &mut S) -> io::Result<Vec<u8>>
where
    S: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 1024];
    while find_header_end(&bytes).is_none() {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "stream ended before HTTP headers",
            ));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(bytes)
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

pub struct PlainH1Origin {
    pub address: SocketAddr,
    requests: Arc<AtomicUsize>,
    http_bytes: Arc<AtomicUsize>,
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<Result<(), BoxError>>>,
}

impl PlainH1Origin {
    pub async fn start(body: &'static [u8]) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("plain origin bind");
        let address = listener.local_addr().expect("plain origin address");
        let requests = Arc::new(AtomicUsize::new(0));
        let requests_for_task = Arc::clone(&requests);
        let http_bytes = Arc::new(AtomicUsize::new(0));
        let http_bytes_for_task = Arc::clone(&http_bytes);
        let (shutdown, mut shutdown_watch) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (stream, _) = accepted?;
                        let requests = Arc::clone(&requests_for_task);
                        let http_bytes = Arc::clone(&http_bytes_for_task);
                        connections.spawn(serve_plain_h1(stream, body, requests, http_bytes));
                    }
                    changed = shutdown_watch.changed() => {
                        changed?;
                        break;
                    }
                    completed = connections.join_next(), if !connections.is_empty() => {
                        completed.expect("connection task exists")??;
                    }
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
            Ok(())
        });
        Self {
            address,
            requests,
            http_bytes,
            shutdown,
            task: Some(task),
        }
    }

    pub async fn wait_for_requests(&self, expected: usize) {
        wait_for_counter(&self.requests, expected, "plain origin requests").await;
    }

    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    pub fn http_bytes(&self) -> usize {
        self.http_bytes.load(Ordering::SeqCst)
    }

    pub async fn finish(mut self) {
        self.shutdown.send(true).expect("signal plain origin");
        finish_origin_task(self.task.take().expect("plain origin task")).await;
    }
}

impl Drop for PlainH1Origin {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn serve_plain_h1(
    mut stream: TcpStream,
    body: &'static [u8],
    requests: Arc<AtomicUsize>,
    http_bytes: Arc<AtomicUsize>,
) -> Result<(), BoxError> {
    loop {
        match read_until_headers_observed(&mut stream, &http_bytes).await {
            Ok(_) => {
                requests.fetch_add(1, Ordering::SeqCst);
                write_h1_response(&mut stream, body).await?;
            }
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
}

async fn write_h1_response<S>(stream: &mut S, body: &[u8]) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}
