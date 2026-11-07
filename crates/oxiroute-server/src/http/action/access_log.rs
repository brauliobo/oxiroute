#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) const ACCESS_LOG_QUEUE_CAPACITY: usize = 1_024;

pub(crate) struct AccessLog {
    pub(super) sender: Option<SyncSender<Vec<u8>>>,
    pub(super) service: String,
    pub(super) rtmp_metrics: Option<Arc<crate::observability::logging::RtmpAccessLogMetrics>>,
    pub(super) worker: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for AccessLog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccessLog")
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

impl AccessLog {
    pub(crate) fn validate(policy: Option<&AccessLogPolicy>) -> Result<(), AccessPreflightError> {
        let Some(AccessLogPolicy::File { path }) = policy else {
            return Ok(());
        };
        let parent = path.parent().ok_or(AccessPreflightError)?;
        let name = path.file_name().ok_or(AccessPreflightError)?;
        let parent = open_pinned_directory(parent).map_err(|_| AccessPreflightError)?;
        match rustix_fs::openat(
            &parent,
            name,
            OFlags::WRONLY | OFlags::APPEND | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(descriptor) => {
                let metadata = rustix_fs::fstat(&descriptor).map_err(|_| AccessPreflightError)?;
                FileType::from_raw_mode(metadata.st_mode)
                    .is_file()
                    .then_some(())
                    .ok_or(AccessPreflightError)
            }
            Err(Errno::NOENT) => {
                rustix_fs::accessat(&parent, ".", rustix_fs::Access::WRITE_OK, AtFlags::EACCESS)
                    .map_err(|_| AccessPreflightError)
            }
            Err(_) => Err(AccessPreflightError),
        }
    }

    pub(crate) fn open(
        service: &str,
        policy: Option<&AccessLogPolicy>,
    ) -> Result<Option<Self>, AccessPreflightError> {
        Self::open_with_rtmp_metrics(service, policy, None)
    }

    pub(crate) fn open_rtmp(
        service: &str,
        policy: Option<&AccessLogPolicy>,
    ) -> Result<Option<Self>, AccessPreflightError> {
        Self::open_with_rtmp_metrics(
            service,
            policy,
            Some(crate::observability::logging::rtmp_access_log_metrics()),
        )
    }

    fn open_with_rtmp_metrics(
        service: &str,
        policy: Option<&AccessLogPolicy>,
        rtmp_metrics: Option<Arc<crate::observability::logging::RtmpAccessLogMetrics>>,
    ) -> Result<Option<Self>, AccessPreflightError> {
        let Some(AccessLogPolicy::File { path }) = policy else {
            return Ok(None);
        };
        let parent = path.parent().ok_or(AccessPreflightError)?;
        let name = path.file_name().ok_or(AccessPreflightError)?;
        let parent = open_pinned_directory(parent).map_err(|_| AccessPreflightError)?;
        let descriptor = rustix_fs::openat(
            &parent,
            name,
            OFlags::WRONLY | OFlags::APPEND | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|_| AccessPreflightError)?;
        let metadata = rustix_fs::fstat(&descriptor).map_err(|_| AccessPreflightError)?;
        if !FileType::from_raw_mode(metadata.st_mode).is_file() {
            return Err(AccessPreflightError);
        }
        let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(ACCESS_LOG_QUEUE_CAPACITY);
        let worker_metrics = rtmp_metrics.clone();
        let worker_name = if rtmp_metrics.is_some() {
            "rtmp-access-log"
        } else {
            "http-access-log"
        };
        let worker = std::thread::Builder::new()
            .name(format!("{worker_name}-{service}"))
            .spawn(move || {
                let mut file = File::from(descriptor);
                while let Ok(line) = receiver.recv() {
                    if let Some(metrics) = &worker_metrics {
                        metrics.worker_received();
                    }
                    if file.write_all(&line).is_err() || file.write_all(b"\n").is_err() {
                        if let Some(metrics) = &worker_metrics {
                            metrics.worker_failed();
                            for _ in receiver.try_iter() {
                                metrics.worker_received();
                            }
                        }
                        break;
                    }
                    if let Some(metrics) = &worker_metrics {
                        metrics.worker_written();
                    }
                }
            })
            .map_err(|_| AccessPreflightError)?;
        Ok(Some(Self {
            sender: Some(sender),
            service: service.to_owned(),
            rtmp_metrics,
            worker: Mutex::new(Some(worker)),
        }))
    }

    pub(crate) fn write(&self, event: &serde_json::Value) -> std::io::Result<()> {
        self.enqueue(serde_json::to_vec(
            &crate::observability::logging::redact_access_record(event),
        )?)
    }

    pub(crate) fn write_rtmp(&self, event: &serde_json::Value) -> std::io::Result<()> {
        self.enqueue(serde_json::to_vec(
            &crate::observability::logging::redact_rtmp_access_record(event),
        )?)
    }

    fn enqueue(&self, line: Vec<u8>) -> std::io::Result<()> {
        if let Some(metrics) = &self.rtmp_metrics {
            metrics.queue_event();
        }
        self.sender
            .as_ref()
            .expect("access log sender exists until final drop")
            .try_send(line)
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    if let Some(metrics) = &self.rtmp_metrics {
                        metrics.queue_event_rejected(true);
                    }
                    std::io::Error::new(std::io::ErrorKind::WouldBlock, "access log queue is full")
                }
                TrySendError::Disconnected(_) => {
                    if let Some(metrics) = &self.rtmp_metrics {
                        metrics.queue_event_rejected(false);
                    }
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "access log writer stopped")
                }
            })
    }

    pub(crate) fn service(&self) -> &str {
        &self.service
    }
}

impl Drop for AccessLog {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self
            .worker
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = worker.join();
        }
    }
}
