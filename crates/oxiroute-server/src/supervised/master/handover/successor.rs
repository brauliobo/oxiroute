use super::wire::{self, Commit, Offer, Reply};
#[allow(clippy::wildcard_imports)]
use super::*;

/// The link of a master started by another to the master it succeeds.
pub(super) struct Successor {
    writer: SeqpacketEndpoint,
    released: Receiver<()>,
    done: bool,
}

impl Successor {
    pub(super) fn start(
        endpoint: SeqpacketEndpoint,
        waker: Arc<Waker>,
    ) -> Result<Self, Box<dyn Error>> {
        let descriptor = endpoint.into_owned_fd();
        let mut reader = SeqpacketEndpoint::from_owned_fd(rustix::io::fcntl_dupfd_cloexec(
            descriptor.as_fd(),
            0,
        )?)?;
        let (released_by, released) = mpsc::channel();
        thread::Builder::new()
            .name("oxiroute-successor-link".into())
            .spawn(move || {
                let _ = wire::receive::<Commit>(&mut reader);
                let _ = released_by.send(());
                waker.wake();
            })?;
        Ok(Self {
            writer: SeqpacketEndpoint::from_owned_fd(descriptor)?,
            released,
            done: false,
        })
    }

    pub(super) fn report_ready(&mut self) -> Result<(), Box<dyn Error>> {
        wire::send(&mut self.writer, &Reply::Ready, &[])
    }

    /// Whether the previous master committed the hand-over, or is gone.
    pub(super) fn predecessor_done(&self) -> bool {
        self.done || self.released.try_recv().is_ok()
    }
}

/// Runs the master that a running master started to succeed it: it serves the listeners the
/// previous master sent, and tells it when its worker is ready.
pub(in crate::supervised) fn run_successor(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<(), Box<dyn Error>> {
    oxiroute_server::init_logging();
    oxiroute_server::init_build_id()?;
    let config_path = PathBuf::from(
        arguments
            .next()
            .ok_or("the successor needs a configuration")?,
    );
    let mut endpoint = SeqpacketEndpoint::from_owned_fd(rustix::io::fcntl_dupfd_cloexec(
        rustix::stdio::stdin(),
        3,
    )?)?;
    match inherit(&mut endpoint, &config_path) {
        Ok((coordinator, document, reservations)) => MasterRunner::production()?.run_with(
            &coordinator,
            &document,
            reservations,
            Some(endpoint),
        ),
        Err(error) => {
            let reason = error.to_string();
            let _ = wire::send(&mut endpoint, &Reply::Failed { reason }, &[]);
            Err(error)
        }
    }
}

fn inherit(
    endpoint: &mut SeqpacketEndpoint,
    config_path: &Path,
) -> Result<
    (
        CanonicalConfigCoordinator,
        Box<ResolvedConfigDocument>,
        ListenerReservations,
    ),
    Box<dyn Error>,
> {
    let (offer, descriptors) = wire::receive::<Offer>(endpoint)?;
    offer.check()?;
    let coordinator = CanonicalConfigCoordinator::new(config_path)?;
    let document = load_document(&coordinator)?;
    supervised_eligibility(&document.validated_config, probe_cgroup_v2().status)?;
    let set = DescriptorSet::new(&offer.manifest, descriptors)?;
    let reservations =
        ListenerReservations::inherit(&document.validated_config, set).map_err(|error| {
            format!(
                "the configuration on disk no longer matches the listeners being served; \
                 reload it first: {error}"
            )
        })?;
    Ok((coordinator, document, reservations))
}
