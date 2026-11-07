#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VodRange {
    pub start: u64,
    pub end: u64,
}

impl VodRange {
    #[must_use]
    pub const fn full(size: u64) -> Option<Self> {
        if size == 0 {
            None
        } else {
            Some(Self {
                start: 0,
                end: size - 1,
            })
        }
    }

    /// Parses one RFC 9110 byte range. Multiple ranges are rejected to keep response memory and
    /// framing bounded to one contiguous body.
    ///
    /// # Errors
    ///
    /// Returns [`VodError::InvalidRange`] when the range is malformed, empty, unsatisfiable, or
    /// contains multiple ranges.
    pub fn parse(value: Option<&str>, size: u64) -> Result<Option<Self>, VodError> {
        let Some(value) = value else {
            return Ok(Self::full(size));
        };
        if size == 0 || !value.starts_with("bytes=") {
            return Err(VodError::InvalidRange);
        }
        let value = &value[6..];
        if value.contains(',') {
            return Err(VodError::InvalidRange);
        }
        let (start, end) = value.split_once('-').ok_or(VodError::InvalidRange)?;
        if start.is_empty() {
            let suffix = end.parse::<u64>().map_err(|_| VodError::InvalidRange)?;
            if suffix == 0 {
                return Err(VodError::InvalidRange);
            }
            let length = suffix.min(size);
            return Ok(Some(Self {
                start: size - length,
                end: size - 1,
            }));
        }
        let start = start.parse::<u64>().map_err(|_| VodError::InvalidRange)?;
        if start >= size {
            return Err(VodError::InvalidRange);
        }
        let end = if end.is_empty() {
            size - 1
        } else {
            end.parse::<u64>()
                .map_err(|_| VodError::InvalidRange)?
                .min(size - 1)
        };
        if end < start {
            return Err(VodError::InvalidRange);
        }
        Ok(Some(Self { start, end }))
    }

    #[must_use]
    pub const fn length(self) -> u64 {
        self.end - self.start + 1
    }
}

#[derive(Clone)]
pub(super) struct VodRoot {
    fd: Arc<OwnedFd>,
}

impl VodRoot {
    pub(super) fn open(path: &Path) -> Result<Self, VodError> {
        let fd = rustix_fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|_| VodError::RootOpen)?;
        let metadata = rustix_fs::fstat(&fd).map_err(|_| VodError::RootOpen)?;
        if !FileType::from_raw_mode(metadata.st_mode).is_dir() || metadata.st_mode & 0o022 != 0 {
            return Err(VodError::RootOpen);
        }
        Ok(Self { fd: Arc::new(fd) })
    }

    pub(super) fn read(&self, path: &str, maximum: u64) -> Result<Vec<u8>, VodError> {
        let components = validated_path(path)?;
        let mut parent = None;
        let descriptor = components
            .iter()
            .enumerate()
            .try_fold(None, |_, (index, component)| {
                let directory = match parent.as_ref() {
                    Some(directory) => directory,
                    None => self.fd.as_ref(),
                };
                let flags = if index + 1 == components.len() {
                    OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW
                } else {
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW
                };
                let opened = rustix_fs::openat(directory, component.as_str(), flags, Mode::empty())
                    .map_err(|error| {
                        if error == rustix::io::Errno::NOENT {
                            VodError::NotFound
                        } else {
                            VodError::Fetch
                        }
                    })?;
                if index + 1 == components.len() {
                    Ok(Some(opened))
                } else {
                    parent = Some(opened);
                    Ok(None)
                }
            })?
            .ok_or(VodError::NotFound)?;
        let metadata = rustix_fs::fstat(&descriptor).map_err(|_| VodError::NotFound)?;
        if !FileType::from_raw_mode(metadata.st_mode).is_file() {
            return Err(VodError::NotFound);
        }
        let size = u64::try_from(metadata.st_size).map_err(|_| VodError::TooLarge)?;
        if size > maximum {
            return Err(VodError::TooLarge);
        }
        let mut file = File::from(descriptor);
        let mut bytes = Vec::with_capacity(usize::try_from(size).map_err(|_| VodError::TooLarge)?);
        file.read_to_end(&mut bytes).map_err(|_| VodError::Fetch)?;
        if u64::try_from(bytes.len()).map_err(|_| VodError::TooLarge)? > maximum {
            return Err(VodError::TooLarge);
        }
        Ok(bytes)
    }
}

pub(super) fn validated_path(path: &str) -> Result<Vec<String>, VodError> {
    if path.is_empty()
        || path.len() > MAX_VOD_PATH_BYTES
        || path.starts_with('/')
        || path.contains("//")
    {
        return Err(VodError::InvalidPath);
    }
    let mut components = Vec::new();
    for component in Path::new(path).components() {
        let Component::Normal(component) = component else {
            return Err(VodError::InvalidPath);
        };
        let component = component.to_str().ok_or(VodError::InvalidPath)?;
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.contains(['/', '\\', '?', '#', '%'])
            || component.chars().any(char::is_control)
            || !component.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(VodError::InvalidPath);
        }
        components.push(component.to_owned());
    }
    if components.is_empty() {
        return Err(VodError::InvalidPath);
    }
    Ok(components)
}
