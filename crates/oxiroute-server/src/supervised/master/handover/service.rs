use std::{
    io::{BufRead as _, BufReader, Read as _, Write as _},
    os::unix::net::{UnixListener, UnixStream},
    sync::mpsc::Sender,
};

use rustix::event::{PollFd, PollFlags, poll};

#[allow(clippy::wildcard_imports)]
use super::*;

const COMMAND_READ_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_COMMAND_BYTES: u64 = 4096;

/// The connection of one `oxiroute upgrade` client, which follows the hand-over line by line.
pub(super) struct Client(UnixStream);

impl Client {
    /// A client that went away does not stop the hand-over, so a failed write is dropped.
    pub(super) fn send(&mut self, event: &UpgradeEvent) {
        if let Ok(mut line) = serde_json::to_vec(event) {
            line.push(b'\n');
            let _ = self.0.write_all(&line);
        }
    }
}

pub(super) struct UpgradeRequest {
    pub(super) client: Client,
    pub(super) command: UpgradeCommand,
}

/// Accepts upgrade requests on the abstract socket named after the configuration.
pub(super) struct UpgradeService {
    requests: Receiver<UpgradeRequest>,
    stop: OwnedFd,
    thread: Option<JoinHandle<()>>,
}

impl UpgradeService {
    pub(super) fn start(config_path: &Path, waker: Arc<Waker>) -> io::Result<Self> {
        let listener = UnixListener::bind_addr(&control_address(config_path)?)?;
        listener.set_nonblocking(true)?;
        let stop = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)?;
        let stopped = stop.try_clone()?;
        let (sender, requests) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("oxiroute-upgrade".into())
            .spawn(move || accept_requests(&listener, &stopped, &sender, &waker))?;
        Ok(Self {
            requests,
            stop,
            thread: Some(thread),
        })
    }

    pub(super) fn next(&self) -> Option<UpgradeRequest> {
        self.requests.try_recv().ok()
    }
}

impl Drop for UpgradeService {
    fn drop(&mut self) {
        let _ = rustix::io::write(&self.stop, &1_u64.to_ne_bytes());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn accept_requests(
    listener: &UnixListener,
    stop: &OwnedFd,
    requests: &Sender<UpgradeRequest>,
    waker: &Waker,
) {
    loop {
        let mut ready = [
            PollFd::new(stop, PollFlags::IN),
            PollFd::new(listener, PollFlags::IN),
        ];
        match poll(&mut ready, None) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => {
                error!("the upgrade socket stopped: {error}");
                return;
            }
        }
        if !ready[0].revents().is_empty() {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(request) = read_request(stream) {
                    if requests.send(request).is_err() {
                        return;
                    }
                    waker.wake();
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => {
                error!("the upgrade socket stopped: {error}");
                return;
            }
        }
    }
}

fn read_request(stream: UnixStream) -> Option<UpgradeRequest> {
    let peer = rustix::net::sockopt::socket_peercred(stream.as_fd()).ok()?;
    let mut client = Client(stream);
    if peer.uid != rustix::process::geteuid() && !peer.uid.is_root() {
        client.send(&UpgradeEvent::Refused);
        return None;
    }
    client.0.set_read_timeout(Some(COMMAND_READ_TIMEOUT)).ok()?;
    let mut line = String::new();
    BufReader::new((&client.0).take(MAX_COMMAND_BYTES))
        .read_line(&mut line)
        .ok()?;
    let command = serde_json::from_str(&line).ok()?;
    Some(UpgradeRequest { client, command })
}
