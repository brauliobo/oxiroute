use std::{
    io,
    process::Child,
    sync::{
        OnceLock,
        mpsc::{self, Sender},
    },
    thread,
};

use super::REAP_POLL_INTERVAL;

static REAPER: OnceLock<Reaper> = OnceLock::new();

struct Reaper {
    sender: Option<Sender<Child>>,
    startup_error: Option<String>,
}

fn reaper() -> &'static Reaper {
    REAPER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<Child>();
        match thread::Builder::new()
            .name(String::from("oxiroute-worker-reaper"))
            .spawn(move || {
                let mut children = Vec::<Child>::new();
                loop {
                    match receiver.recv_timeout(REAP_POLL_INTERVAL) {
                        Ok(child) => children.push(child),
                        Err(mpsc::RecvTimeoutError::Disconnected) if children.is_empty() => break,
                        Err(
                            mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected,
                        ) => {}
                    }
                    while let Ok(child) = receiver.try_recv() {
                        children.push(child);
                    }
                    let mut index = 0;
                    while index < children.len() {
                        match children[index].try_wait() {
                            Ok(Some(_)) => {
                                let mut reaped = children.swap_remove(index);
                                let _ = reaped.wait();
                            }
                            Ok(None) | Err(_) => index += 1,
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

pub(super) fn submit_to_reaper(child: Child) {
    let Some(sender) = &reaper().sender else {
        // Worker creation calls ensure_reaper before spawning, so this is unreachable for an owned
        // child. Leaking is safer than Child::drop creating an immediately unreapable zombie.
        std::mem::forget(child);
        return;
    };
    if let Err(error) = sender.send(child) {
        // The dedicated receiver loop contains no panic path. If it nevertheless terminated, do
        // not make Drop unbounded; retain the OS child resource until process exit.
        std::mem::forget(error.0);
    }
}
