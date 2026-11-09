#[allow(clippy::wildcard_imports)]
use super::*;

mod sink;

use crate::label::Label;
pub(super) use sink::QUEUE_CAPACITY_LINES as ACCESS_LOG_QUEUE_CAPACITY;
use sink::Rejected;
pub(super) use sink::Sink;

std::thread_local! {
    static LINE: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub(crate) struct AccessLog {
    pub(super) sink: Arc<Sink>,
    pub(super) service: String,
    pub(super) logged_service: Label,
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

    pub(super) fn open_with_rtmp_metrics(
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
        let sink = Arc::new(Sink::new(ACCESS_LOG_QUEUE_CAPACITY));
        let worker_name = if rtmp_metrics.is_some() {
            "rtmp-access-log"
        } else {
            "http-access-log"
        };
        let worker = std::thread::Builder::new()
            .name(format!("{worker_name}-{service}"))
            .spawn({
                let (sink, metrics) = (Arc::clone(&sink), rtmp_metrics.clone());
                move || write_batches(&sink, File::from(descriptor), metrics.as_deref())
            })
            .map_err(|_| AccessPreflightError)?;
        Ok(Some(Self {
            sink,
            service: service.to_owned(),
            logged_service: Label::new(service),
            rtmp_metrics,
            worker: Mutex::new(Some(worker)),
        }))
    }

    /// Redacts a generic event to the HTTP allowlist and queues it.
    pub(crate) fn write(&self, event: &serde_json::Value) -> std::io::Result<()> {
        self.write_record(&crate::observability::logging::redact_access_record(event))
    }

    pub(crate) fn write_rtmp(&self, event: &serde_json::Value) -> std::io::Result<()> {
        self.write_record(&crate::observability::logging::redact_rtmp_access_record(
            event,
        ))
    }

    /// Serializes `record` into a reused per-thread buffer and queues the line.
    pub(crate) fn write_record(&self, record: &impl serde::Serialize) -> std::io::Result<()> {
        self.write_with(|line| Ok(serde_json::to_writer(line, record)?))
    }

    /// Renders one line with `render` into the reused per-thread buffer and queues it.
    pub(crate) fn write_with(
        &self,
        render: impl FnOnce(&mut Vec<u8>) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        LINE.with_borrow_mut(|line| {
            line.clear();
            render(line)?;
            line.push(b'\n');
            self.enqueue(line)
        })
    }

    fn enqueue(&self, line: &[u8]) -> std::io::Result<()> {
        if let Some(metrics) = &self.rtmp_metrics {
            metrics.queue_event();
        }
        self.sink.push(line).map_err(|rejected| {
            if let Some(metrics) = &self.rtmp_metrics {
                metrics.queue_event_rejected(rejected == Rejected::Full);
            }
            match rejected {
                Rejected::Full => {
                    std::io::Error::new(std::io::ErrorKind::WouldBlock, "access log queue is full")
                }
                Rejected::Stopped => {
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "access log writer stopped")
                }
            }
        })
    }

    pub(crate) fn service(&self) -> &str {
        &self.service
    }

    /// The service name as every access line writes it, redacted once.
    pub(crate) fn logged_service(&self) -> &str {
        self.logged_service.logged()
    }
}

impl Drop for AccessLog {
    fn drop(&mut self) {
        self.sink.close();
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

/// Writes each batch with one call; the first failure stops the log and counts what it lost.
fn write_batches(
    sink: &Sink,
    mut file: File,
    metrics: Option<&crate::observability::logging::RtmpAccessLogMetrics>,
) {
    let mut batch = Vec::new();
    while let Some(lines) = sink.next_batch(&mut batch) {
        if let Some(metrics) = metrics {
            metrics.worker_received(lines);
        }
        if file.write_all(&batch).is_err() {
            if let Some(metrics) = metrics {
                metrics.worker_failed();
                metrics.worker_received(sink.fail());
            } else {
                sink.fail();
            }
            return;
        }
        if let Some(metrics) = metrics {
            metrics.worker_written(lines);
        }
    }
}
