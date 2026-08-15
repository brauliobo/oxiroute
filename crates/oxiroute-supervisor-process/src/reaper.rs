use std::{
    io,
    process::Child,
    sync::{
        OnceLock,
        mpsc::{self, Sender},
    },
    thread,
};

use super::{REAP_POLL_INTERVAL, cgroup::WorkerCgroupLease};

static REAPER: OnceLock<Reaper> = OnceLock::new();

struct Reaper {
    sender: Option<Sender<ReaperEntry>>,
    startup_error: Option<String>,
}

struct ReaperEntry {
    child: Option<Child>,
    cgroup: Option<WorkerCgroupLease>,
}

fn reaper() -> &'static Reaper {
    REAPER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<ReaperEntry>();
        match thread::Builder::new()
            .name(String::from("oxiroute-worker-reaper"))
            .spawn(move || {
                let mut children = Vec::<ReaperEntry>::new();
                loop {
                    if children.is_empty() {
                        let Ok(child) = receiver.recv() else {
                            break;
                        };
                        children.push(child);
                    } else {
                        match receiver.recv_timeout(REAP_POLL_INTERVAL) {
                            Ok(child) => children.push(child),
                            Err(
                                mpsc::RecvTimeoutError::Timeout
                                | mpsc::RecvTimeoutError::Disconnected,
                            ) => {}
                        }
                    }
                    while let Ok(child) = receiver.try_recv() {
                        children.push(child);
                    }
                    let mut index = 0;
                    while index < children.len() {
                        let child_complete = match children[index].child.as_mut() {
                            Some(child) => matches!(child.try_wait(), Ok(Some(_))),
                            None => true,
                        };
                        if child_complete {
                            let mut reaped = children.swap_remove(index);
                            if let Some(child) = reaped.child.as_mut() {
                                let _ = child.wait();
                            }
                            reaped.child = None;
                            if !reaped.cleanup_complete() {
                                children.push(reaped);
                                index += 1;
                            }
                        } else {
                            index += 1;
                        }
                    }
                }
            }) {
            Ok(_) => Reaper {
                sender: Some(sender),
                startup_error: None,
            },
            Err(error) => Reaper {
                sender: None,
                startup_error: Some(error.to_string()),
            },
        }
    })
}

pub(super) fn ensure_reaper() -> io::Result<()> {
    match &reaper().startup_error {
        Some(error) => Err(io::Error::other(format!(
            "worker reaper thread failed to start: {error}"
        ))),
        None => Ok(()),
    }
}

pub(super) fn submit_to_reaper(child: Child, cgroup: Option<WorkerCgroupLease>) {
    submit(ReaperEntry {
        child: Some(child),
        cgroup,
    });
}

pub(super) fn submit_cgroup_to_reaper(cgroup: WorkerCgroupLease) {
    submit(ReaperEntry {
        child: None,
        cgroup: Some(cgroup),
    });
}

fn submit(entry: ReaperEntry) {
    let Some(sender) = &reaper().sender else {
        // Worker creation calls ensure_reaper before spawning, so this is unreachable for an owned
        // entry. Leaking is safer than creating an immediately unreapable zombie or stale owner.
        std::mem::forget(entry);
        return;
    };
    if let Err(error) = sender.send(entry) {
        // The dedicated receiver loop contains no panic path. If it nevertheless terminated, do
        // not make Drop unbounded; retain the OS child and cgroup ownership until process exit.
        std::mem::forget(error.0);
    }
}

impl ReaperEntry {
    fn cleanup_complete(&mut self) -> bool {
        if self.child.is_some() {
            return false;
        }
        let Some(cgroup) = self.cgroup.as_mut() else {
            return true;
        };
        let _ = cgroup.kill();
        match cgroup.cleanup() {
            Ok(true) => {
                self.cgroup = None;
                true
            }
            Ok(false) | Err(_) => false,
        }
    }
}
