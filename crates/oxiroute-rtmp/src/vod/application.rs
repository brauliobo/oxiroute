#[allow(clippy::wildcard_imports)]
use super::*;

pub struct VodObject {
    source: String,
    path: String,
    bytes: Arc<[u8]>,
    max_duration: Duration,
    _lease: VodLease,
}

impl VodObject {
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub fn len(&self) -> u64 {
        u64::try_from(self.bytes.len()).unwrap_or(u64::MAX)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub const fn max_duration(&self) -> Duration {
        self.max_duration
    }

    /// Returns one validated contiguous byte range from the object.
    ///
    /// # Errors
    ///
    /// Returns [`VodError::InvalidRange`] when the range does not fit this object.
    pub fn range(&self, range: VodRange) -> Result<Vec<u8>, VodError> {
        let start = usize::try_from(range.start).map_err(|_| VodError::InvalidRange)?;
        let end = usize::try_from(range.end).map_err(|_| VodError::InvalidRange)?;
        if end < start || end >= self.bytes.len() {
            return Err(VodError::InvalidRange);
        }
        Ok(self.bytes[start..=end].to_vec())
    }
}

pub struct VodApplication {
    service: String,
    application: String,
    limits: VodLimits,
    sources: BTreeMap<String, VodSource>,
    active_sessions: Arc<AtomicUsize>,
}

#[derive(Clone, Debug)]
pub struct VodApplicationBlueprint {
    service: String,
    application: String,
    limits: VodLimits,
    sources: BTreeMap<String, VodSourceBlueprint>,
    policy: RtmpOutboundPolicy,
}

#[derive(Clone, Debug)]
enum VodSourceBlueprint {
    Local(std::path::PathBuf),
    Http(HttpOriginBlueprint),
}

enum VodSource {
    Local(VodRoot),
    Http(HttpOrigin),
}

impl VodApplication {
    /// Creates a preflighted VOD application and pins all local roots and HTTP origins.
    ///
    /// # Errors
    ///
    /// Returns an error when a source root, source origin, limit, or source name is invalid.
    pub fn new(
        service: impl Into<String>,
        application: impl Into<String>,
        limits: VodLimits,
        sources: impl IntoIterator<Item = VodSourceDefinition>,
        policy: &RtmpOutboundPolicy,
    ) -> Result<Self, VodError> {
        let blueprint =
            VodApplicationBlueprint::compile(service, application, limits, sources, policy)?;
        Self::acquire(&blueprint)
    }

    /// Opens roots and HTTP clients for a compiled VOD application.
    ///
    /// # Errors
    ///
    /// Returns an acquisition error when a root or origin cannot be prepared.
    pub fn acquire(blueprint: &VodApplicationBlueprint) -> Result<Self, VodError> {
        let mut compiled = BTreeMap::new();
        for (name, source) in &blueprint.sources {
            let source = match source {
                VodSourceBlueprint::Local(root_directory) => {
                    VodSource::Local(VodRoot::open(root_directory)?)
                }
                VodSourceBlueprint::Http(origin) => {
                    VodSource::Http(HttpOrigin::acquire(origin, &blueprint.policy)?)
                }
            };
            compiled.insert(name.clone(), source);
        }
        Ok(Self {
            service: blueprint.service.clone(),
            application: blueprint.application.clone(),
            limits: blueprint.limits,
            sources: compiled,
            active_sessions: Arc::new(AtomicUsize::new(0)),
        })
    }
}

impl VodApplicationBlueprint {
    /// Compiles immutable VOD source and limit decisions without opening resources.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid limits, source identities, paths, or origin syntax.
    pub fn compile(
        service: impl Into<String>,
        application: impl Into<String>,
        limits: VodLimits,
        sources: impl IntoIterator<Item = VodSourceDefinition>,
        policy: &RtmpOutboundPolicy,
    ) -> Result<Self, VodError> {
        let mut compiled = BTreeMap::new();
        for source in sources {
            source
                .validate_intrinsic()
                .map_err(|_| VodError::InvalidPath)?;
            let name = source.name().to_owned();
            if !valid_source_name(&name) {
                return Err(VodError::InvalidPath);
            }
            let source = match source {
                VodSourceDefinition::Local { root_directory, .. } => {
                    VodSourceBlueprint::Local(root_directory)
                }
                VodSourceDefinition::Http { origin, .. } => {
                    VodSourceBlueprint::Http(HttpOriginBlueprint::parse(&origin)?)
                }
            };
            if compiled.insert(name, source).is_some() {
                return Err(VodError::SourceNotFound);
            }
        }
        if limits.max_sessions == 0 || limits.max_file_bytes == 0 || limits.max_duration.is_zero() {
            return Err(VodError::SessionLimit);
        }
        Ok(Self {
            service: service.into(),
            application: application.into(),
            limits,
            sources: compiled,
            policy: policy.clone(),
        })
    }
}

impl VodApplication {
    #[must_use]
    pub fn service(&self) -> &str {
        &self.service
    }

    #[must_use]
    pub fn application(&self) -> &str {
        &self.application
    }

    #[must_use]
    pub const fn limits(&self) -> VodLimits {
        self.limits
    }

    /// Opens one bounded VOD object and acquires its concurrent-session lease.
    ///
    /// # Errors
    ///
    /// Returns an error when the source/path is invalid, the object cannot be loaded, or the
    /// concurrent-session limit is reached.
    pub fn open(&self, source: &str, path: &str) -> Result<VodObject, VodError> {
        self.validate_request(source, path)?;
        let lease = self.reserve()?;
        let (bytes, max_duration) = self.load(source, path)?;
        Ok(VodObject {
            source: source.to_owned(),
            path: path.to_owned(),
            bytes: bytes.into(),
            max_duration,
            _lease: lease,
        })
    }

    pub(crate) fn validate_request(&self, source: &str, path: &str) -> Result<(), VodError> {
        if !valid_source_name(source) {
            return Err(VodError::InvalidPath);
        }
        if !self.sources.contains_key(source) {
            return Err(VodError::SourceNotFound);
        }
        validated_path(path).map(|_| ())
    }

    pub(crate) fn reserve(&self) -> Result<VodLease, VodError> {
        VodLease::acquire(Arc::clone(&self.active_sessions), self.limits.max_sessions)
    }

    pub(crate) fn load(&self, source: &str, path: &str) -> Result<(Vec<u8>, Duration), VodError> {
        let bytes = match self.sources.get(source).ok_or(VodError::SourceNotFound)? {
            VodSource::Local(root) => root.read(path, self.limits.max_file_bytes)?,
            VodSource::Http(origin) => origin.fetch(path, self.limits.max_file_bytes)?,
        };
        Ok((bytes, self.limits.max_duration))
    }
}

pub(super) fn valid_source_name(value: &str) -> bool {
    (1..=MAX_VOD_SOURCE_NAME_BYTES).contains(&value.len())
        && value.bytes().all(|byte| byte.is_ascii_graphic())
        && !value.contains(['/', '\\', '?', '#', '%'])
        && value != "."
        && value != ".."
}

type VodApplicationMap = BTreeMap<(String, String), Arc<VodApplication>>;

#[derive(Clone, Default)]
pub struct VodCatalog {
    applications: Arc<Mutex<VodApplicationMap>>,
}

impl VodCatalog {
    #[must_use]
    pub(crate) fn from_applications(
        applications: impl IntoIterator<Item = Arc<VodApplication>>,
    ) -> Arc<Self> {
        let applications = applications
            .into_iter()
            .map(|application| {
                (
                    (application.service.clone(), application.application.clone()),
                    application,
                )
            })
            .collect();
        Arc::new(Self {
            applications: Arc::new(Mutex::new(applications)),
        })
    }

    /// Opens one object from the named service/application catalog entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the service, application, source, path, or object is unavailable.
    ///
    /// # Panics
    ///
    /// Panics if the catalog mutex is poisoned.
    pub fn open(
        &self,
        service: &str,
        application: &str,
        source: &str,
        path: &str,
    ) -> Result<VodObject, VodError> {
        let applications = self
            .applications
            .lock()
            .expect("VOD catalog mutex poisoned");
        applications
            .get(&(service.to_owned(), application.to_owned()))
            .ok_or(VodError::SourceNotFound)?
            .open(source, path)
    }

    /// Returns a cloned application entry when it exists.
    ///
    /// # Panics
    ///
    /// Panics if the catalog mutex is poisoned.
    #[must_use]
    pub fn application(&self, service: &str, application: &str) -> Option<Arc<VodApplication>> {
        self.applications
            .lock()
            .expect("VOD catalog mutex poisoned")
            .get(&(service.to_owned(), application.to_owned()))
            .cloned()
    }
}

pub struct VodLease {
    active_sessions: Arc<AtomicUsize>,
}

impl VodLease {
    fn acquire(active_sessions: Arc<AtomicUsize>, maximum: usize) -> Result<Self, VodError> {
        let mut active = active_sessions.load(Ordering::Acquire);
        loop {
            if active >= maximum {
                return Err(VodError::SessionLimit);
            }
            match active_sessions.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(Self { active_sessions }),
                Err(current) => active = current,
            }
        }
    }
}

impl Drop for VodLease {
    fn drop(&mut self) {
        self.active_sessions.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) fn parse_flv_events(
    bytes: &[u8],
    maximum_duration: Duration,
) -> Result<Vec<MediaEvent>, VodError> {
    if bytes.len() < 13 || &bytes[..3] != b"FLV" || bytes[3] != 1 {
        return Err(VodError::InvalidFlv);
    }
    let data_offset = u32::from_be_bytes(bytes[5..9].try_into().expect("FLV header is checked"));
    let mut offset = usize::try_from(data_offset).map_err(|_| VodError::InvalidFlv)?;
    if offset < 9 || offset.checked_add(4).is_none_or(|end| end > bytes.len()) {
        return Err(VodError::InvalidFlv);
    }
    offset += 4;
    let maximum_ms = u32::try_from(maximum_duration.as_millis()).unwrap_or(u32::MAX);
    let mut events = Vec::new();
    while offset < bytes.len() {
        if offset.checked_add(11).is_none_or(|end| end > bytes.len()) {
            return Err(VodError::InvalidFlv);
        }
        let tag_type = bytes[offset];
        let data_size = usize::try_from(u32::from_be_bytes([
            0,
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]))
        .map_err(|_| VodError::InvalidFlv)?;
        let timestamp = u32::from_be_bytes([
            bytes[offset + 7],
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
        ]);
        let data_start = offset + 11;
        let data_end = data_start
            .checked_add(data_size)
            .and_then(|end| end.checked_add(4))
            .ok_or(VodError::InvalidFlv)?;
        if data_end > bytes.len() {
            return Err(VodError::InvalidFlv);
        }
        let previous_size = u32::from_be_bytes(
            bytes[data_start + data_size..data_end]
                .try_into()
                .expect("FLV previous tag size is checked"),
        );
        if previous_size != u32::try_from(data_size + 11).map_err(|_| VodError::InvalidFlv)? {
            return Err(VodError::InvalidFlv);
        }
        if timestamp <= maximum_ms {
            let payload: Arc<[u8]> = bytes[data_start..data_start + data_size].into();
            let event = match tag_type {
                8 => MediaEvent::audio(timestamp, payload).map_err(|_| VodError::InvalidMedia)?,
                9 => MediaEvent::video(timestamp, payload).map_err(|_| VodError::InvalidMedia)?,
                _ => {
                    offset = data_end;
                    continue;
                }
            };
            events.push(event);
            if events.len() > MAX_VOD_EVENTS {
                return Err(VodError::TooLarge);
            }
        }
        offset = data_end;
    }
    Ok(events)
}
