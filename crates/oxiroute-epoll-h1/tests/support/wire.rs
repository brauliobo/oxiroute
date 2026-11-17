use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};

pub struct Wire<T = TcpStream> {
    pub stream: T,
    pending: Vec<u8>,
}

pub struct Reply {
    pub status: u16,
    pub head: String,
    pub body: Vec<u8>,
    pub raw_body: Vec<u8>,
}

impl Reply {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn header(&self, name: &str) -> Option<String> {
        self.head.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    }
}

impl Wire {
    pub fn connect(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).expect("connect to the engine");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("read timeout");
        stream.set_nodelay(true).expect("nodelay");
        Self::over(stream)
    }

    pub fn silent_for(&mut self, limit: Duration) -> bool {
        self.stream
            .set_read_timeout(Some(limit))
            .expect("read timeout");
        let silent = !self.fill();
        self.stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("read timeout");
        silent
    }
}

impl<T: Read + Write> Wire<T> {
    pub fn over(stream: T) -> Self {
        Self {
            stream,
            pending: Vec::new(),
        }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).expect("send");
    }

    pub fn get(&mut self, path: &str) {
        self.send(format!("GET {path} HTTP/1.1\r\nHost: test.example\r\n\r\n").as_bytes());
    }

    fn fill(&mut self) -> bool {
        let mut chunk = vec![0_u8; 65536];
        match self.stream.read(&mut chunk) {
            Ok(count) if count > 0 => {
                self.pending.extend_from_slice(&chunk[..count]);
                true
            }
            _ => false,
        }
    }

    fn take(&mut self, count: usize) -> Vec<u8> {
        while self.pending.len() < count {
            assert!(self.fill(), "the connection ended inside a message");
        }
        self.pending.drain(..count).collect()
    }

    fn line(&mut self) -> String {
        loop {
            if let Some(at) = self.pending.windows(2).position(|window| window == b"\r\n") {
                let line = self.pending.drain(..at + 2).collect::<Vec<_>>();
                return String::from_utf8_lossy(&line[..at]).into_owned();
            }
            assert!(self.fill(), "the connection ended inside a line");
        }
    }

    pub fn reply(&mut self) -> Reply {
        self.reply_to(false)
    }

    pub fn head(&mut self) -> Reply {
        let mut head = String::new();
        loop {
            let line = self.line();
            if line.is_empty() {
                break;
            }
            head.push_str(&line);
            head.push('\n');
        }
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .expect("status");
        Reply {
            status,
            head,
            body: Vec::new(),
            raw_body: Vec::new(),
        }
    }

    pub fn reply_to(&mut self, head_request: bool) -> Reply {
        let mut reply = self.head();
        let status = reply.status;
        let bodiless = head_request || matches!(status, 204 | 304);
        if bodiless {
            return reply;
        }
        if reply
            .header("transfer-encoding")
            .is_some_and(|value| value == "chunked")
        {
            self.chunked_body(&mut reply);
        } else if let Some(length) = reply.header("content-length") {
            reply.body = self.take(length.parse().expect("length"));
        } else {
            while self.fill() {}
            reply.body = std::mem::take(&mut self.pending);
        }
        reply
    }

    fn chunked_body(&mut self, reply: &mut Reply) {
        loop {
            let size_line = self.line();
            reply
                .raw_body
                .extend_from_slice(format!("{size_line}\r\n").as_bytes());
            let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("0"), 16)
                .expect("chunk size");
            if size == 0 {
                loop {
                    let trailer = self.line();
                    reply
                        .raw_body
                        .extend_from_slice(format!("{trailer}\r\n").as_bytes());
                    if trailer.is_empty() {
                        return;
                    }
                }
            }
            let data = self.take(size + 2);
            reply.raw_body.extend_from_slice(&data);
            reply.body.extend_from_slice(&data[..size]);
        }
    }

    pub fn rest(&mut self) -> Vec<u8> {
        while self.fill() {}
        std::mem::take(&mut self.pending)
    }

    pub fn ended(&mut self) -> bool {
        self.pending.is_empty() && !self.fill()
    }
}
