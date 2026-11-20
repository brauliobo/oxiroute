use std::{
    io::{BufRead as _, BufReader, Write as _},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
};

use zeroize::Zeroizing;

use crate::cli::net::{Client, Endpoint, Timeouts};

const TOKEN: &str = "0123456789abcdefghijklmnopqrstuv";

pub(super) struct Route {
    path: &'static str,
    status: u16,
    bodies: Vec<String>,
    hits: AtomicUsize,
}

impl Route {
    pub(super) fn ok(path: &'static str, body: &serde_json::Value) -> Self {
        Self::answering(path, 200, body)
    }

    pub(super) fn answering(path: &'static str, status: u16, body: &serde_json::Value) -> Self {
        Self {
            path,
            status,
            bodies: vec![body.to_string()],
            hits: AtomicUsize::new(0),
        }
    }

    pub(super) fn sequence(path: &'static str, bodies: &[serde_json::Value]) -> Self {
        Self {
            path,
            status: 200,
            bodies: bodies.iter().map(ToString::to_string).collect(),
            hits: AtomicUsize::new(0),
        }
    }

    fn body(&self) -> String {
        let hit = self.hits.fetch_add(1, Ordering::SeqCst);
        self.bodies[hit.min(self.bodies.len() - 1)].clone()
    }
}

pub(super) struct Fixture {
    address: SocketAddr,
    seen: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

fn answer(mut stream: TcpStream, routes: &[Route], seen: &Mutex<Vec<String>>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut head = String::new();
    reader.read_line(&mut head).expect("request line");
    let target = head
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let mut authorized = false;
    let mut line = String::new();
    while reader.read_line(&mut line).expect("header") > 2 {
        authorized |= line.trim() == format!("Authorization: Bearer {TOKEN}");
        line.clear();
    }
    seen.lock().expect("seen").push(target.clone());
    let path = target.split('?').next().unwrap_or_default();
    let (status, body) = match routes.iter().find(|route| route.path == path) {
        _ if !authorized => (401, r#"{"error":{"code":"unauthorized"}}"#.to_owned()),
        Some(route) => (route.status, route.body()),
        None => (404, r#"{"error":{"code":"not_found"}}"#.to_owned()),
    };
    let response = format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).expect("response");
}

impl Fixture {
    pub(super) fn serve(routes: Vec<Route>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let (seen, stop) = (Arc::clone(&seen), Arc::clone(&stop));
            thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    answer(stream.expect("connection"), &routes, &seen);
                }
            })
        };
        Self {
            address,
            seen,
            stop,
            handle: Some(handle),
        }
    }

    pub(super) fn client(&self) -> Client {
        Client::new(
            Endpoint::parse(&format!("http://{}", self.address)).expect("endpoint"),
            Some(Zeroizing::new(TOKEN.to_owned())),
            Timeouts::DEFAULT,
        )
    }

    pub(super) fn requested(&self) -> Vec<String> {
        self.seen.lock().expect("seen").clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(handle) = self.handle.take() {
            handle.join().expect("the fixture server stopped");
        }
    }
}
