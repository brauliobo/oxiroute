use std::{
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

pub struct Origin {
    pub addr: SocketAddr,
    pub accepted: Arc<AtomicUsize>,
    pub flaky_left: Arc<AtomicUsize>,
    connections: Arc<Mutex<Vec<TcpStream>>>,
    pub heads: Arc<Mutex<Vec<String>>>,
}

struct Incoming {
    method: String,
    path: String,
    head: String,
    body: Vec<u8>,
}

impl Origin {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind origin");
        let origin = Self {
            addr: listener.local_addr().expect("origin address"),
            accepted: Arc::default(),
            flaky_left: Arc::new(AtomicUsize::new(1)),
            connections: Arc::default(),
            heads: Arc::default(),
        };
        let shared = (
            Arc::clone(&origin.accepted),
            Arc::clone(&origin.connections),
            Arc::clone(&origin.heads),
            Arc::clone(&origin.flaky_left),
        );
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                shared.0.fetch_add(1, Ordering::SeqCst);
                stream.set_nodelay(true).expect("nodelay");
                shared
                    .1
                    .lock()
                    .expect("connections")
                    .push(stream.try_clone().expect("clone"));
                let (heads, flaky) = (Arc::clone(&shared.2), Arc::clone(&shared.3));
                thread::spawn(move || serve(stream, &heads, &flaky));
            }
        });
        origin
    }

    pub fn scripted(script: impl Fn(TcpStream) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind origin");
        let origin = Self {
            addr: listener.local_addr().expect("origin address"),
            accepted: Arc::default(),
            flaky_left: Arc::default(),
            connections: Arc::default(),
            heads: Arc::default(),
        };
        let (accepted, script) = (Arc::clone(&origin.accepted), Arc::new(script));
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                accepted.fetch_add(1, Ordering::SeqCst);
                let script = Arc::clone(&script);
                thread::spawn(move || script(stream));
            }
        });
        origin
    }

    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    pub fn close_connections(&self) {
        for stream in self.connections.lock().expect("connections").drain(..) {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

pub fn await_head(stream: &mut TcpStream) {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut chunk = [0_u8; 1];
        if stream
            .read(&mut chunk)
            .ok()
            .filter(|count| *count > 0)
            .is_none()
        {
            return;
        }
        head.push(chunk[0]);
    }
}

pub fn reset(stream: TcpStream) {
    rustix::net::sockopt::set_socket_linger(&stream, Some(Duration::ZERO)).expect("linger");
    drop(stream);
}

fn read_request(stream: &mut TcpStream, pending: &mut Vec<u8>) -> Option<Incoming> {
    let end = loop {
        if let Some(at) = pending.windows(4).position(|window| window == b"\r\n\r\n") {
            break at + 4;
        }
        let mut chunk = [0_u8; 8192];
        let count = stream.read(&mut chunk).ok().filter(|count| *count > 0)?;
        pending.extend_from_slice(&chunk[..count]);
    };
    let head = String::from_utf8_lossy(&pending[..end]).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::trim)
                .map(str::to_owned)
        })
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let method = head.split_whitespace().next().unwrap_or("GET").to_owned();
    let early = path.starts_with("/early");
    pending.drain(..end);
    let mut body = Vec::new();
    while !early && body.len() < length {
        if pending.is_empty() {
            let mut chunk = [0_u8; 65536];
            let count = stream.read(&mut chunk).ok().filter(|count| *count > 0)?;
            pending.extend_from_slice(&chunk[..count]);
        }
        let take = pending.len().min(length - body.len());
        body.extend(pending.drain(..take));
    }
    Some(Incoming {
        method,
        path,
        head,
        body,
    })
}

fn serve(mut stream: TcpStream, heads: &Mutex<Vec<String>>, flaky: &AtomicUsize) {
    let mut pending = Vec::new();
    while let Some(request) = read_request(&mut stream, &mut pending) {
        heads.lock().expect("heads").push(request.head.clone());
        if !respond(&mut stream, &request, flaky) {
            let _ = stream.shutdown(Shutdown::Both);
            return;
        }
    }
}

fn sized(len: usize) -> Vec<u8> {
    (0..len).map(|index| b'a' + (index % 23) as u8).collect()
}

fn ok(body: &[u8], extra: &str) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n{extra}\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

fn chunk(data: &[u8]) -> Vec<u8> {
    let mut out = format!("{:x}\r\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\r\n");
    out
}

fn write(stream: &mut TcpStream, bytes: &[u8]) -> bool {
    stream.write_all(bytes).is_ok()
}

fn respond(stream: &mut TcpStream, request: &Incoming, flaky: &AtomicUsize) -> bool {
    let path = request.path.as_str();
    let segment = path.rsplit('/').next().unwrap_or("");
    let number = segment.parse::<usize>().unwrap_or(0);
    match path.split('/').nth(1).unwrap_or("") {
        "size" => write(stream, &ok(&sized(number), "")),
        "echo" => write(stream, &ok(&request.body, "")),
        "info" => write(stream, &ok(request.head.as_bytes(), "")),
        "once" => {
            let _ = write(stream, &ok(b"once", "Connection: keep-alive\r\n"));
            false
        }
        "chunked" => {
            let mut out = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
            (0..number.max(1))
                .for_each(|index| out.extend(chunk(format!("chunk-{index};").as_bytes())));
            out.extend_from_slice(b"0\r\nX-Trailer: yes\r\n\r\n");
            write(stream, &out)
        }
        "until-close" => {
            let _ = write(
                stream,
                b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\npart-one;",
            );
            thread::sleep(Duration::from_millis(30));
            let _ = write(stream, b"part-two;");
            false
        }
        "status" if matches!(number, 204 | 304) => write(
            stream,
            format!("HTTP/1.1 {number} Status\r\nX-Kind: empty\r\n\r\n").as_bytes(),
        ),
        "status" => write(
            stream,
            format!("HTTP/1.1 {number} Status\r\nContent-Length: 0\r\n\r\n").as_bytes(),
        ),
        "head" => write(stream, b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n"),
        "slowbody" => {
            let _ = write(stream, b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nab");
            thread::sleep(Duration::from_millis(number as u64));
            write(stream, b"cd")
        }
        "delay" => {
            thread::sleep(Duration::from_millis(number as u64));
            write(stream, &ok(b"late", ""))
        }
        "stall" => {
            let _ = write(
                stream,
                b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n0123456789",
            );
            thread::sleep(Duration::from_secs(3));
            false
        }
        "early" => {
            let _ = write(
                stream,
                b"HTTP/1.1 413 Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            false
        }
        "closed" => false,
        "garbage" => write(stream, b"HTTP/1.1 20x Broken\r\n\r\n") && false,
        "hugehead" => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nX-Big: {}\r\nContent-Length: 0\r\n\r\n",
                "b".repeat(30_000)
            );
            write(stream, head.as_bytes())
        }
        "flaky" => {
            let failing = flaky
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_ok();
            let status = if failing { "503 Busy" } else { "200 OK" };
            write(
                stream,
                format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n").as_bytes(),
            )
        }
        _ => write(
            stream,
            &ok(b"ok", &format!("X-Method: {}\r\n", request.method)),
        ),
    }
}
