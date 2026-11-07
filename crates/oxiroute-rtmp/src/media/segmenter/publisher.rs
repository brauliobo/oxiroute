#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Default)]
pub struct MediaApplication {
    hls: Option<Arc<HlsOutputConfig>>,
    dash: Option<Arc<DashOutputConfig>>,
}

impl MediaApplication {
    #[must_use]
    pub fn new(hls: Option<Arc<HlsOutputConfig>>) -> Self {
        Self { hls, dash: None }
    }

    #[must_use]
    pub fn hls(&self) -> Option<&Arc<HlsOutputConfig>> {
        self.hls.as_ref()
    }

    #[must_use]
    pub fn with_dash(mut self, dash: Option<Arc<DashOutputConfig>>) -> Self {
        self.dash = dash;
        self
    }

    #[must_use]
    pub fn dash(&self) -> Option<&Arc<DashOutputConfig>> {
        self.dash.as_ref()
    }

    pub(crate) fn attach(
        &self,
        key: &StreamKey,
        incarnation: PublisherIncarnation,
    ) -> Result<Option<MediaPublisher>, MediaOutputError> {
        if self.hls.is_none() && self.dash.is_none() {
            return Ok(None);
        }
        MediaPublisher::start(key, incarnation, self).map(Some)
    }
}

#[derive(Clone, Default)]
pub struct MediaCatalog {
    applications: Arc<BTreeMap<(String, String), Arc<MediaApplication>>>,
}

impl MediaCatalog {
    #[must_use]
    pub(crate) fn from_applications(
        applications: impl IntoIterator<Item = (String, String, Arc<MediaApplication>)>,
    ) -> Self {
        Self {
            applications: Arc::new(
                applications
                    .into_iter()
                    .map(|(service, application, media)| ((service, application), media))
                    .collect(),
            ),
        }
    }

    #[must_use]
    pub fn merge(catalogs: impl IntoIterator<Item = Arc<Self>>) -> Self {
        let mut applications = BTreeMap::new();
        for catalog in catalogs {
            applications.extend(
                catalog
                    .applications
                    .iter()
                    .map(|(key, value)| (key.clone(), Arc::clone(value))),
            );
        }
        Self {
            applications: Arc::new(applications),
        }
    }

    pub(crate) fn read(
        &self,
        service: &str,
        application: &str,
        stream: &str,
        object: &str,
    ) -> Result<MediaObject, MediaStoreError> {
        let media = self
            .applications
            .get(&(service.to_owned(), application.to_owned()))
            .ok_or(MediaStoreError::NotFound)?;
        let key = StreamKey::new(service, application, stream);
        if let Some(dash) = &media.dash
            && let Some(prefix) = dash.store.current_prefix(&key)
        {
            match resolve_dash_public_path(dash, &prefix, object) {
                Ok((path, content_type)) => {
                    let body = dash
                        .store
                        .read_relative(&path, dash.store.limits().max_file_bytes)?;
                    return Ok(MediaObject { body, content_type });
                }
                Err(MediaStoreError::InvalidPath) => {
                    return Err(MediaStoreError::InvalidPath);
                }
                Err(MediaStoreError::NotFound) => {}
                Err(error) => return Err(error),
            }
        }
        if let Some(hls) = &media.hls {
            let prefix = hls
                .store
                .current_prefix(&key)
                .ok_or(MediaStoreError::NotFound)?;
            let (path, content_type) = resolve_public_path(hls, &prefix, object)?;
            let body = hls
                .store
                .read_relative(&path, hls.store.limits().max_file_bytes)?;
            return Ok(MediaObject { body, content_type });
        }
        Err(MediaStoreError::NotFound)
    }

    /// Reads one bounded public HLS or DASH object for an active publisher incarnation.
    ///
    /// # Errors
    ///
    /// Returns an error when the application, stream, or object is unavailable, invalid, or
    /// exceeds the configured file bound.
    pub fn read_object(
        &self,
        service: &str,
        application: &str,
        stream: &str,
        object: &str,
    ) -> Result<MediaObject, MediaStoreError> {
        self.read(service, application, stream, object)
    }
}

pub struct MediaObject {
    pub body: Vec<u8>,
    pub content_type: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaEnqueueResult {
    Queued,
    Dropped,
    Closed,
}

#[derive(Debug, thiserror::Error)]
pub enum MediaOutputError {
    #[error("media worker cannot be started")]
    WorkerSpawn(#[source] std::io::Error),
    #[error("media storage failed")]
    Storage(#[from] MediaStoreError),
}

pub struct MediaPublisher {
    sender: SyncSender<MediaCommand>,
    closed: Arc<AtomicBool>,
}

enum MediaCommand {
    Event {
        event: MediaEvent,
        observed_at_unix_ms: u64,
    },
    Close,
}

enum MediaWorker {
    Hls(HlsSegmenter),
    Dash(DashSegmenter),
}

impl MediaPublisher {
    pub(super) fn start(
        key: &StreamKey,
        incarnation: PublisherIncarnation,
        config: &MediaApplication,
    ) -> Result<Self, MediaOutputError> {
        let hls = config.hls.clone();
        let dash = config.dash.clone();
        let dash_store = dash.as_ref().map(|output| Arc::clone(&output.store));
        let mut stores = Vec::<Arc<MediaStore>>::new();
        for store in hls
            .as_ref()
            .map(|output| Arc::clone(&output.store))
            .into_iter()
            .chain(dash_store.clone())
        {
            if stores.iter().any(|current| Arc::ptr_eq(current, &store)) {
                continue;
            }
            let preserve = dash_store
                .as_ref()
                .is_some_and(|dash_store| Arc::ptr_eq(dash_store, &store));
            let attached = if preserve {
                store.attach_continuing(key, incarnation)
            } else {
                store.attach(key, incarnation)
            };
            if let Err(error) = attached {
                for attached_store in &stores {
                    attached_store.close(key, incarnation);
                }
                return Err(MediaOutputError::Storage(error));
            }
            stores.push(store);
        }
        let queue_messages = hls
            .as_ref()
            .map_or(0, |output| output.max_queue_messages)
            .max(dash.as_ref().map_or(0, |output| output.max_queue_messages));
        let (sender, receiver) = mpsc::sync_channel(queue_messages.max(1));
        let closed = Arc::new(AtomicBool::new(false));
        let worker_key = key.clone();
        let worker_closed = Arc::clone(&closed);
        let mut workers = Vec::new();
        if let Some(hls) = hls {
            let store = Arc::clone(&hls.store);
            workers.push(MediaWorker::Hls(HlsSegmenter::new(
                worker_key.clone(),
                incarnation,
                store,
                hls,
            )));
        }
        if let Some(dash) = dash {
            let store = Arc::clone(&dash.store);
            match DashSegmenter::new(worker_key.clone(), incarnation, store, dash) {
                Ok(segmenter) => workers.push(MediaWorker::Dash(segmenter)),
                Err(error) => {
                    for store in stores {
                        store.close(key, incarnation);
                    }
                    return Err(MediaOutputError::Storage(error));
                }
            }
        }
        thread::Builder::new()
            .name("oxiroute-rtmp-media".into())
            .spawn(move || {
                run_worker(&mut workers, &receiver, &worker_closed);
            })
            .map_err(|error| {
                for store in stores {
                    store.close(key, incarnation);
                }
                MediaOutputError::WorkerSpawn(error)
            })?;
        Ok(Self { sender, closed })
    }

    #[must_use]
    pub fn try_enqueue(&self, event: MediaEvent, observed_at_unix_ms: u64) -> MediaEnqueueResult {
        if self.closed.load(Ordering::Acquire) {
            return MediaEnqueueResult::Closed;
        }
        match self.sender.try_send(MediaCommand::Event {
            event,
            observed_at_unix_ms,
        }) {
            Ok(()) => MediaEnqueueResult::Queued,
            Err(TrySendError::Full(_)) => MediaEnqueueResult::Dropped,
            Err(TrySendError::Disconnected(_)) => MediaEnqueueResult::Closed,
        }
    }

    pub fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.sender.try_send(MediaCommand::Close);
    }
}

impl Drop for MediaPublisher {
    fn drop(&mut self) {
        self.close();
    }
}

fn run_worker(
    workers: &mut [MediaWorker],
    receiver: &Receiver<MediaCommand>,
    closed: &Arc<AtomicBool>,
) {
    while let Ok(command) = receiver.recv() {
        match command {
            MediaCommand::Event {
                event,
                observed_at_unix_ms,
            } => {
                for worker in workers.iter_mut() {
                    match worker {
                        MediaWorker::Hls(segmenter) => {
                            segmenter.accept(&event, observed_at_unix_ms);
                        }
                        MediaWorker::Dash(segmenter) => segmenter.accept(&event),
                    }
                }
                if closed.load(Ordering::Acquire) {
                    finish_workers(workers, true);
                    return;
                }
            }
            MediaCommand::Close => {
                finish_workers(workers, true);
                return;
            }
        }
    }
    finish_workers(workers, true);
}

fn finish_workers(workers: &mut [MediaWorker], end_list: bool) {
    for worker in workers {
        match worker {
            MediaWorker::Hls(segmenter) => segmenter.finish(end_list),
            MediaWorker::Dash(segmenter) => segmenter.finish(end_list),
        }
    }
}
