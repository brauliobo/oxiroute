use std::{
    ffi::OsString,
    os::fd::{AsFd, BorrowedFd},
    process::{Child, Stdio},
    sync::mpsc::{self, Receiver},
};

use oxiroute_server::upgrade::{Command as UpgradeCommand, Event as UpgradeEvent, control_address};
use oxiroute_supervision_unix::{DescriptorKind, DescriptorSet, FrameFlags, MessageType};
use oxiroute_supervisor_process::build_id_of;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

#[allow(clippy::wildcard_imports)]
use super::*;

mod notify;
mod predecessor;
mod service;
mod successor;
mod wire;

pub(super) use predecessor::Handed;
use predecessor::{Handover, Progress};
use service::{Client, UpgradeRequest, UpgradeService};
use successor::Successor;
pub(in crate::supervised) use successor::run_successor;

pub(in crate::supervised) const SUCCESSOR_MARKER: &str = "--__oxiroute-successor-3b81c0a7";
const READY_TIMEOUT: Duration = Duration::from_mins(1);

/// What a master offers and awaits around a binary hand-over: the socket on which it takes upgrade
/// requests, the hand-over it runs for a request, and the link to the master it succeeds.
pub(super) struct Upgrades {
    config_path: PathBuf,
    waker: Arc<Waker>,
    service: Option<UpgradeService>,
    pending: Option<Handover>,
    successor: Option<Successor>,
}

struct Plan {
    binary: PathBuf,
    window: Duration,
}

impl Upgrades {
    pub(super) fn start(
        config_path: &Path,
        waker: Arc<Waker>,
        inherited: Option<SeqpacketEndpoint>,
    ) -> Result<Self, Box<dyn Error>> {
        let (service, successor) = match inherited {
            Some(endpoint) => (None, Some(Successor::start(endpoint, Arc::clone(&waker))?)),
            None => (
                Some(UpgradeService::start(config_path, Arc::clone(&waker))?),
                None,
            ),
        };
        Ok(Self {
            config_path: config_path.to_path_buf(),
            waker,
            service,
            pending: None,
            successor,
        })
    }

    /// Tells the master this one succeeds that its worker serves.
    pub(super) fn report_ready(&mut self) -> Result<(), Box<dyn Error>> {
        self.successor
            .as_mut()
            .map_or(Ok(()), Successor::report_ready)
    }

    /// Takes over the upgrade socket once the master this one succeeds has let go of it.
    pub(super) fn take_over_service(&mut self) -> Result<(), Box<dyn Error>> {
        if self
            .successor
            .as_ref()
            .is_some_and(Successor::predecessor_done)
        {
            self.successor = None;
            self.service = Some(UpgradeService::start(
                &self.config_path,
                Arc::clone(&self.waker),
            )?);
        }
        Ok(())
    }

    pub(super) const fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn next_due(&self) -> Option<Instant> {
        self.pending.as_ref().map(Handover::deadline)
    }
}

impl MasterRunner {
    /// Takes the next upgrade request and advances the hand-over it started. Returns the hand-over
    /// once it committed: from then on this master only lets its worker drain.
    pub(super) fn advance_upgrade(
        &self,
        master: &mut Master,
        upgrades: &mut Upgrades,
        catalog: &GenerationCatalog,
        now: Instant,
    ) -> Result<Option<Handed>, Box<dyn Error>> {
        if upgrades.pending.is_none() {
            let request = upgrades.service.as_ref().and_then(UpgradeService::next);
            if let Some(request) = request {
                let began = self.begin_upgrade(master, upgrades, request, catalog, now);
                upgrades.pending = began;
            }
        }
        let Some(pending) = upgrades.pending.as_mut() else {
            return Ok(None);
        };
        match pending.poll(now) {
            Progress::Waiting => Ok(None),
            Progress::Failed(reason) => {
                if let Some(pending) = upgrades.pending.take() {
                    pending.fail(&reason);
                }
                Ok(None)
            }
            Progress::Ready => match upgrades.pending.take() {
                Some(pending) => Self::commit_upgrade(master, upgrades, pending, now),
                None => Ok(None),
            },
        }
    }

    fn begin_upgrade(
        &self,
        master: &Master,
        upgrades: &Upgrades,
        request: UpgradeRequest,
        catalog: &GenerationCatalog,
        now: Instant,
    ) -> Option<Handover> {
        let UpgradeRequest {
            mut client,
            command,
        } = request;
        match self.plan_upgrade(master, &command, catalog) {
            Ok(plan) => Handover::begin(
                master,
                &plan,
                &upgrades.config_path,
                client,
                (Arc::clone(&upgrades.waker), now),
            ),
            Err(reason) => {
                client.send(&UpgradeEvent::Failed { reason });
                None
            }
        }
    }

    fn plan_upgrade(
        &self,
        master: &Master,
        command: &UpgradeCommand,
        catalog: &GenerationCatalog,
    ) -> Result<Plan, String> {
        if master.state() != MasterState::Running || catalog.candidate().is_some() {
            return Err("a configuration reload is in progress; retry when it has finished".into());
        }
        if notify::unit_ignores_main_pid_changes() {
            return Err(
                "the systemd unit does not let the master change its main process: add \
                 NotifyAccess=main to its [Service] section"
                    .into(),
            );
        }
        if master
            .listener_manifest()
            .slots()
            .iter()
            .any(|slot| slot.kind == DescriptorKind::UnixListener)
        {
            return Err(
                "a configuration with Unix socket listeners cannot be handed over: the socket \
                 file and its ownership marker stay with this master; restart the service"
                    .into(),
            );
        }
        let binary = command
            .binary
            .clone()
            .unwrap_or_else(|| self.executable_path.clone());
        let binary = std::fs::canonicalize(&binary)
            .map_err(|error| format!("cannot use `{}`: {error}", binary.display()))?;
        let window = retirement_window(
            &catalog.active().payload().config,
            std::env::var_os(RELOAD_DRAIN_ENV).as_deref(),
        ) + RETIREMENT_BACKSTOP;
        Ok(Plan { binary, window })
    }

    fn commit_upgrade(
        master: &mut Master,
        upgrades: &mut Upgrades,
        handover: Handover,
        now: Instant,
    ) -> Result<Option<Handed>, Box<dyn Error>> {
        if let Err(error) = notify::announce_main_pid(handover.pid()) {
            handover.fail(&format!(
                "systemd did not accept the new main process: {error}"
            ));
            return Ok(None);
        }
        master.hand_over(handover.window(), now)?;
        upgrades.service = None;
        Ok(Some(handover.commit()))
    }

    /// Lets the worker this master handed over drain, then ends the master.
    pub(super) fn finish_handover(
        master: &mut Master,
        signals: &SignalMonitor,
        handed: Handed,
    ) -> Result<(), Box<dyn Error>> {
        info!("supervised master handed its listeners over and waits for its worker to drain");
        loop {
            if signals.stop.load(Ordering::Acquire) {
                return shutdown_master(master);
            }
            let events = master.poll(Instant::now())?;
            log_master_events(&events);
            match master.state() {
                MasterState::Stopped => {
                    handed.drained();
                    return Ok(());
                }
                MasterState::Failed => return Err(master_failure("handover").into()),
                _ => signals.wait(master, None)?,
            }
        }
    }
}
