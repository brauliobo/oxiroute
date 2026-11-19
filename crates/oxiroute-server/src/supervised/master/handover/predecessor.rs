use std::{os::unix::process::ExitStatusExt as _, process::Command, sync::mpsc::TryRecvError};

use oxiroute_server::upgrade::Event;

use super::wire::{self, Commit, Offer, Reply};
#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) enum Progress {
    Waiting,
    Ready,
    Failed(String),
}

/// The successor's process, ended with SIGTERM unless the hand-over committed.
struct SuccessorProcess(Option<Child>);

impl SuccessorProcess {
    fn release(mut self) -> Child {
        self.0.take().expect("the successor process is owned")
    }

    fn id(&self) -> u32 {
        self.0.as_ref().map_or(0, Child::id)
    }

    fn lost_link(&mut self, link: &str) -> String {
        let Some(mut child) = self.0.take() else {
            return format!("the successor stopped answering: {link}");
        };
        let _ = child.kill();
        match child.wait() {
            Ok(status) if status.signal() != Some(rustix::process::Signal::KILL.as_raw()) => {
                format!("the successor exited before it was ready: {status}")
            }
            _ => format!("the successor stopped answering: {link}"),
        }
    }
}

impl Drop for SuccessorProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = rustix::process::kill_process(
                rustix::process::Pid::from_child(&child),
                rustix::process::Signal::TERM,
            );
            let _ = thread::Builder::new()
                .name("oxiroute-successor-reap".into())
                .spawn(move || {
                    let _ = child.wait();
                });
        }
    }
}

struct Started {
    process: SuccessorProcess,
    writer: SeqpacketEndpoint,
    replies: Receiver<Result<Reply, String>>,
}

/// A successor master started from another executable, until it serves or fails.
pub(super) struct Handover {
    process: SuccessorProcess,
    writer: SeqpacketEndpoint,
    replies: Receiver<Result<Reply, String>>,
    client: Client,
    window: Duration,
    deadline: Instant,
}

/// A hand-over that committed: the successor serves, and this master waits for its own worker.
pub(in crate::supervised::master) struct Handed {
    _child: Child,
    client: Client,
}

impl Handed {
    pub(in crate::supervised::master) fn drained(mut self) {
        self.client.send(&Event::Drained);
    }
}

impl Handover {
    pub(super) fn begin(
        master: &Master,
        plan: &Plan,
        config_path: &Path,
        mut client: Client,
        (waker, now): (Arc<Waker>, Instant),
    ) -> Option<Self> {
        match start(master, plan, config_path, &mut client, waker) {
            Ok(Started {
                process,
                writer,
                replies,
            }) => Some(Self {
                process,
                writer,
                replies,
                client,
                window: plan.window,
                deadline: now + READY_TIMEOUT,
            }),
            Err(reason) => {
                client.send(&Event::Failed { reason });
                None
            }
        }
    }

    pub(super) const fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(super) const fn window(&self) -> Duration {
        self.window
    }

    pub(super) fn pid(&self) -> u32 {
        self.process.id()
    }

    pub(super) fn poll(&mut self, now: Instant) -> Progress {
        match self.replies.try_recv() {
            Ok(Ok(Reply::Ready)) => Progress::Ready,
            Ok(Ok(Reply::Failed { reason })) => Progress::Failed(reason),
            Ok(Err(link)) => Progress::Failed(self.process.lost_link(&link)),
            Err(TryRecvError::Disconnected) => {
                Progress::Failed(self.process.lost_link("link closed"))
            }
            Err(TryRecvError::Empty) if now >= self.deadline => Progress::Failed(format!(
                "the successor was not ready within {} seconds",
                READY_TIMEOUT.as_secs()
            )),
            Err(TryRecvError::Empty) => Progress::Waiting,
        }
    }

    /// Reports the failure; the successor ends and this master keeps serving.
    pub(super) fn fail(mut self, reason: &str) {
        self.client.send(&Event::Failed {
            reason: reason.to_owned(),
        });
    }

    /// Lets the successor take upgrade requests and tells the client that it serves.
    pub(super) fn commit(self) -> Handed {
        let Self {
            process,
            mut writer,
            mut client,
            ..
        } = self;
        let _ = wire::send(&mut writer, &Commit, &[]);
        client.send(&Event::Serving { pid: process.id() });
        Handed {
            _child: process.release(),
            client,
        }
    }
}

fn start(
    master: &Master,
    plan: &Plan,
    config_path: &Path,
    client: &mut Client,
    waker: Arc<Waker>,
) -> Result<Started, String> {
    let build_id = build_id_of(&plan.binary)
        .map_err(|error| format!("cannot read `{}`: {error}", plan.binary.display()))?;
    client.send(&Event::Starting {
        binary: plan.binary.clone(),
        build_id,
        previous_build_id: oxiroute_server::build_id().unwrap_or_default().to_owned(),
    });
    let (mine, theirs) = SeqpacketEndpoint::pair().map_err(|error| error.to_string())?;
    let child = Command::new(&plan.binary)
        .arg(SUCCESSOR_MARKER)
        .arg(config_path)
        .stdin(Stdio::from(theirs.into_owned_fd()))
        .spawn()
        .map_err(|error| format!("cannot start `{}`: {error}", plan.binary.display()))?;
    let mut process = SuccessorProcess(Some(child));
    let descriptor = mine.into_owned_fd();
    let reading =
        rustix::io::fcntl_dupfd_cloexec(&descriptor, 0).map_err(|error| error.to_string())?;
    let mut writer =
        SeqpacketEndpoint::from_owned_fd(descriptor).map_err(|error| error.to_string())?;
    let listeners = master
        .export_listeners()
        .map_err(|error| error.to_string())?;
    let borrowed: Vec<BorrowedFd<'_>> = listeners.iter().map(AsFd::as_fd).collect();
    let offer = Offer::new(master.listener_manifest().clone());
    wire::send(&mut writer, &offer, &borrowed)
        .map_err(|error| process.lost_link(&error.to_string()))?;
    let reader = SeqpacketEndpoint::from_owned_fd(reading).map_err(|error| error.to_string())?;
    let replies = read_replies(reader, waker).map_err(|error| error.to_string())?;
    Ok(Started {
        process,
        writer,
        replies,
    })
}

fn read_replies(
    mut reader: SeqpacketEndpoint,
    waker: Arc<Waker>,
) -> io::Result<Receiver<Result<Reply, String>>> {
    let (sender, replies) = mpsc::channel();
    thread::Builder::new()
        .name("oxiroute-handover-link".into())
        .spawn(move || {
            let reply = wire::receive::<Reply>(&mut reader)
                .map(|(reply, _)| reply)
                .map_err(|error| error.to_string());
            let _ = sender.send(reply);
            waker.wake();
        })?;
    Ok(replies)
}

#[cfg(test)]
mod tests {
    use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};

    use super::*;

    fn spawn(script: &str) -> SuccessorProcess {
        let child = Command::new("sh")
            .args(["-c", script])
            .spawn()
            .expect("spawn sh");
        SuccessorProcess(Some(child))
    }

    #[test]
    fn a_successor_that_exited_is_reported_with_its_status() {
        let mut process = spawn("exit 3");
        let pid = Pid::from_child(process.0.as_ref().expect("child"));
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT;
        waitid(WaitId::Pid(pid), options).expect("wait for the exit");
        assert_eq!(
            process.lost_link("link closed"),
            "the successor exited before it was ready: exit status: 3"
        );
    }

    #[test]
    fn a_silent_successor_is_ended_and_reported_as_not_answering() {
        let mut process = spawn("exec sleep 60");
        assert_eq!(
            process.lost_link("link closed"),
            "the successor stopped answering: link closed"
        );
        assert!(process.0.is_none());
    }
}
