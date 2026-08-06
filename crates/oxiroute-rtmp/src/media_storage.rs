use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::{PublisherIncarnation, StreamKey};

pub const MAX_MEDIA_PATH_BYTES: usize = 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MediaStoreLimits {
    pub max_bytes: u64,
    pub max_files: usize,
    pub max_active_streams: usize,
    pub max_file_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MediaStoreStats {
    pub bytes_used: u64,
    pub files: usize,
    pub active_streams: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum MediaStoreError {
    #[error("media root cannot be opened")]
    RootOpen(#[source] io::Error),
    #[error("media root is not an exclusive directory")]
    RootNotExclusive,
    #[error("media root cannot be scanned")]
    RootScan(#[source] io::Error),
    #[error("media root already exceeds its configured quota")]
    ExistingUsageExceedsQuota,
    #[error("media stream limit reached")]
    ActiveStreamLimit,
    #[error("media path is invalid")]
    InvalidPath,
    #[error("media file is too large")]
    FileTooLarge,
    #[error("media storage quota reached")]
    Quota,
    #[error("media publisher incarnation is no longer current")]
    StaleIncarnation,
    #[error("media object does not exist")]
    NotFound,
    #[error("media object cannot be read")]
    Read(#[source] io::Error),
    #[error("media manifest is malformed")]
    ManifestMalformed,
    #[error("media object cannot be published")]
    Publish(#[source] io::Error),
    #[error("media object cleanup failed")]
    Cleanup(#[source] io::Error),
}

#[derive(Clone)]
pub struct MediaStore {
    shared: Arc<MediaStoreShared>,
}

struct MediaStoreShared {
    root: PathBuf,
    limits: MediaStoreLimits,
    state: Mutex<MediaStoreState>,
    next_temporary_name: AtomicU64,
}

#[derive(Default)]
struct MediaStoreState {
    stats: MediaStoreStats,
    streams: HashMap<StreamKey, ActiveMediaStream>,
}

#[derive(Clone)]
struct ActiveMediaStream {
    incarnation: PublisherIncarnation,
    relative_prefix: PathBuf,
}

impl MediaStore {
    /// Opens a bounded media root and accounts for regular files already present below it.
    ///
    /// The root may be created on first use. Existing files are retained until the corresponding
    /// stream incarnation is attached, which makes process restarts conservative rather than
    /// deleting a publisher's last complete playlist during startup.
    ///
    /// # Errors
    ///
    /// Returns an error when the root is not a private directory, cannot be scanned, or already
    /// exceeds the configured quota.
    pub fn open(root: impl AsRef<Path>, limits: MediaStoreLimits) -> Result<Self, MediaStoreError> {
        if limits.max_bytes == 0
            || limits.max_files == 0
            || limits.max_active_streams == 0
            || limits.max_file_bytes == 0
        {
            return Err(MediaStoreError::Quota);
        }
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(MediaStoreError::RootOpen)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                .map_err(MediaStoreError::RootOpen)?;
        }
        verify_root(&root)?;
        let stats = scan_root(&root, limits)?;
        if stats.bytes_used > limits.max_bytes || stats.files > limits.max_files {
            return Err(MediaStoreError::ExistingUsageExceedsQuota);
        }
        Ok(Self {
            shared: Arc::new(MediaStoreShared {
                root,
                limits,
                state: Mutex::new(MediaStoreState {
                    stats,
                    streams: HashMap::new(),
                }),
                next_temporary_name: AtomicU64::new(0),
            }),
        })
    }

    /// Scans an existing media root without creating or modifying it.
    ///
    /// # Errors
    ///
    /// Returns an error when the root is not a private directory, cannot be scanned, or exceeds
    /// the configured quota.
    pub fn preflight(
        root: impl AsRef<Path>,
        limits: MediaStoreLimits,
    ) -> Result<MediaStoreStats, MediaStoreError> {
        let root = root.as_ref();
        if !root.exists() {
            return Ok(MediaStoreStats::default());
        }
        verify_root(root)?;
        let stats = scan_root(root, limits)?;
        if stats.bytes_used > limits.max_bytes || stats.files > limits.max_files {
            return Err(MediaStoreError::ExistingUsageExceedsQuota);
        }
        Ok(stats)
    }

    #[must_use]
    pub fn limits(&self) -> MediaStoreLimits {
        self.shared.limits
    }

    #[must_use]
    pub fn stats(&self) -> MediaStoreStats {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stats
    }

    pub(crate) fn attach(
        &self,
        key: &StreamKey,
        incarnation: PublisherIncarnation,
    ) -> Result<PathBuf, MediaStoreError> {
        self.attach_inner(key, incarnation, false)
    }

    /// Attaches a publisher while retaining the previous incarnation's media tree.
    ///
    /// The retained tree is renamed to the new incarnation path, so stale writers still fail the
    /// incarnation check while a DASH worker can recover its manifest and sequence numbers.
    pub(crate) fn attach_continuing(
        &self,
        key: &StreamKey,
        incarnation: PublisherIncarnation,
    ) -> Result<PathBuf, MediaStoreError> {
        self.attach_inner(key, incarnation, true)
    }

    fn attach_inner(
        &self,
        key: &StreamKey,
        incarnation: PublisherIncarnation,
        preserve: bool,
    ) -> Result<PathBuf, MediaStoreError> {
        let mut state = self.lock_state();
        let can_reuse = state.streams.contains_key(key);
        if !can_reuse && state.stats.active_streams >= self.shared.limits.max_active_streams {
            return Err(MediaStoreError::ActiveStreamLimit);
        }

        let stream_prefix = stream_prefix(key)?;
        let relative_prefix = stream_prefix.join(format!("i{}", incarnation.value()));
        if preserve {
            let previous_prefix = state
                .streams
                .get(key)
                .map(|stream| stream.relative_prefix.clone())
                .or(find_latest_incarnation(&self.shared.root, &stream_prefix)?);
            create_directories(
                &self.shared.root,
                relative_prefix.parent().unwrap_or(Path::new("")),
            )?;
            if let Some(previous_prefix) = previous_prefix {
                let previous_path = safe_join(&self.shared.root, &previous_prefix)?;
                let next_path = safe_join(&self.shared.root, &relative_prefix)?;
                if previous_prefix != relative_prefix {
                    fs::rename(previous_path, next_path).map_err(MediaStoreError::Cleanup)?;
                }
            } else {
                create_directories(&self.shared.root, &relative_prefix)?;
            }
        } else {
            remove_tree(&self.shared.root.join(&stream_prefix))?;
            create_directories(&self.shared.root, &relative_prefix)?;
        }
        state.streams.insert(
            key.clone(),
            ActiveMediaStream {
                incarnation,
                relative_prefix: relative_prefix.clone(),
            },
        );
        state.stats = scan_root(&self.shared.root, self.shared.limits)?;
        state.stats.active_streams = state.streams.len();
        Ok(relative_prefix)
    }

    pub(crate) fn close(&self, key: &StreamKey, incarnation: PublisherIncarnation) {
        let mut state = self.lock_state();
        if state
            .streams
            .get(key)
            .is_some_and(|stream| stream.incarnation == incarnation)
        {
            state.streams.remove(key);
            state.stats.active_streams = state.streams.len();
        }
    }

    pub(crate) fn current_prefix(&self, key: &StreamKey) -> Option<PathBuf> {
        self.lock_state()
            .streams
            .get(key)
            .map(|stream| stream.relative_prefix.clone())
    }

    pub(crate) fn publish(
        &self,
        key: &StreamKey,
        incarnation: PublisherIncarnation,
        relative_path: &Path,
        bytes: &[u8],
    ) -> Result<(), MediaStoreError> {
        if bytes.len() > self.shared.limits.max_file_bytes {
            return Err(MediaStoreError::FileTooLarge);
        }
        let mut state = self.lock_state();
        let Some(stream) = state.streams.get(key) else {
            return Err(MediaStoreError::StaleIncarnation);
        };
        if stream.incarnation != incarnation {
            return Err(MediaStoreError::StaleIncarnation);
        }
        let full_path = safe_join(&self.shared.root, relative_path)?;
        let existing_size = match fs::symlink_metadata(&full_path) {
            Ok(metadata) if metadata.file_type().is_file() => Some(metadata.len()),
            Ok(_) => return Err(MediaStoreError::InvalidPath),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(MediaStoreError::Publish(error)),
        };
        let old_bytes = existing_size.unwrap_or(0);
        let new_bytes = state
            .stats
            .bytes_used
            .saturating_sub(old_bytes)
            .checked_add(u64::try_from(bytes.len()).expect("media file size fits in u64"))
            .ok_or(MediaStoreError::Quota)?;
        let new_files = state
            .stats
            .files
            .saturating_sub(usize::from(existing_size.is_some()))
            .checked_add(1)
            .ok_or(MediaStoreError::Quota)?;
        if new_bytes > self.shared.limits.max_bytes || new_files > self.shared.limits.max_files {
            return Err(MediaStoreError::Quota);
        }

        let parent = full_path.parent().ok_or(MediaStoreError::InvalidPath)?;
        create_directories(
            &self.shared.root,
            relative_path.parent().unwrap_or(Path::new("")),
        )?;
        let temporary_name = format!(
            ".oxiroute-media-{}-{}.partial",
            std::process::id(),
            self.shared
                .next_temporary_name
                .fetch_add(1, Ordering::Relaxed)
        );
        let temporary_path = parent.join(temporary_name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;

            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary_path)
            .map_err(MediaStoreError::Publish)?;
        if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(&temporary_path);
            return Err(MediaStoreError::Publish(error));
        }
        drop(file);
        if let Err(error) = fs::rename(&temporary_path, &full_path) {
            let _ = fs::remove_file(&temporary_path);
            return Err(MediaStoreError::Publish(error));
        }
        state.stats.bytes_used = new_bytes;
        state.stats.files = new_files;
        Ok(())
    }

    pub(crate) fn remove(
        &self,
        key: &StreamKey,
        incarnation: PublisherIncarnation,
        relative_path: &Path,
    ) -> Result<(), MediaStoreError> {
        let mut state = self.lock_state();
        let Some(stream) = state.streams.get(key) else {
            return Err(MediaStoreError::StaleIncarnation);
        };
        if stream.incarnation != incarnation {
            return Err(MediaStoreError::StaleIncarnation);
        }
        let full_path = safe_join(&self.shared.root, relative_path)?;
        match fs::symlink_metadata(&full_path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                fs::remove_file(&full_path).map_err(MediaStoreError::Cleanup)?;
                state.stats.bytes_used = state.stats.bytes_used.saturating_sub(metadata.len());
                state.stats.files = state.stats.files.saturating_sub(1);
            }
            Ok(_) => return Err(MediaStoreError::InvalidPath),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(MediaStoreError::Cleanup(error)),
        }
        Ok(())
    }

    pub(crate) fn read_relative(
        &self,
        relative_path: &Path,
        maximum: usize,
    ) -> Result<Vec<u8>, MediaStoreError> {
        let full_path = safe_join(&self.shared.root, relative_path)?;
        let metadata = fs::symlink_metadata(&full_path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                MediaStoreError::NotFound
            } else {
                MediaStoreError::Read(error)
            }
        })?;
        if !metadata.file_type().is_file() {
            return Err(MediaStoreError::InvalidPath);
        }
        let length = usize::try_from(metadata.len()).map_err(|_| MediaStoreError::FileTooLarge)?;
        if length > maximum || length > self.shared.limits.max_file_bytes {
            return Err(MediaStoreError::FileTooLarge);
        }
        let mut file = File::open(full_path).map_err(MediaStoreError::Read)?;
        let mut bytes = Vec::with_capacity(length);
        file.read_to_end(&mut bytes)
            .map_err(MediaStoreError::Read)?;
        Ok(bytes)
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, MediaStoreState> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn verify_root(root: &Path) -> Result<(), MediaStoreError> {
    let metadata = fs::symlink_metadata(root).map_err(MediaStoreError::RootOpen)?;
    if !metadata.file_type().is_dir() {
        return Err(MediaStoreError::RootNotExclusive);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(MediaStoreError::RootNotExclusive);
        }
    }
    Ok(())
}

fn scan_root(root: &Path, limits: MediaStoreLimits) -> Result<MediaStoreStats, MediaStoreError> {
    let mut stats = MediaStoreStats::default();
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        let entries = fs::read_dir(directory).map_err(MediaStoreError::RootScan)?;
        for entry in entries {
            let entry = entry.map_err(MediaStoreError::RootScan)?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(MediaStoreError::RootScan)?;
            if metadata.file_type().is_symlink() {
                return Err(MediaStoreError::RootNotExclusive);
            }
            if metadata.file_type().is_dir() {
                directories.push(path);
            } else if metadata.file_type().is_file() {
                stats.files = stats.files.checked_add(1).ok_or(MediaStoreError::Quota)?;
                stats.bytes_used = stats
                    .bytes_used
                    .checked_add(metadata.len())
                    .ok_or(MediaStoreError::Quota)?;
                if stats.files > limits.max_files || stats.bytes_used > limits.max_bytes {
                    return Ok(stats);
                }
            }
        }
    }
    Ok(stats)
}

fn stream_prefix(key: &StreamKey) -> Result<PathBuf, MediaStoreError> {
    let server = safe_component(&key.server_id)?;
    let application = safe_component(&key.application)?;
    let stream = safe_component(&key.name)?;
    Ok(PathBuf::from(server).join(application).join(stream))
}

fn find_latest_incarnation(
    root: &Path,
    stream_prefix: &Path,
) -> Result<Option<PathBuf>, MediaStoreError> {
    let directory = root.join(stream_prefix);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(MediaStoreError::RootScan(error)),
    };
    let mut latest = None;
    for entry in entries {
        let entry = entry.map_err(MediaStoreError::RootScan)?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(MediaStoreError::RootScan)?;
        if metadata.file_type().is_symlink() {
            return Err(MediaStoreError::RootNotExclusive);
        }
        if !metadata.file_type().is_dir() {
            continue;
        }
        let entry_name = entry.file_name();
        let name = entry_name.to_str().ok_or(MediaStoreError::InvalidPath)?;
        let Some(value) = name
            .strip_prefix('i')
            .and_then(|value| value.parse::<u64>().ok())
        else {
            continue;
        };
        if latest.as_ref().is_none_or(|(current, _)| value > *current) {
            latest = Some((value, stream_prefix.join(name)));
        }
    }
    Ok(latest.map(|(_, path)| path))
}

fn safe_join(root: &Path, relative_path: &Path) -> Result<PathBuf, MediaStoreError> {
    if relative_path.as_os_str().len() > MAX_MEDIA_PATH_BYTES || relative_path.is_absolute() {
        return Err(MediaStoreError::InvalidPath);
    }
    let mut path = root.to_path_buf();
    for component in relative_path.components() {
        let Component::Normal(component) = component else {
            return Err(MediaStoreError::InvalidPath);
        };
        let component = component.to_str().ok_or(MediaStoreError::InvalidPath)?;
        safe_component(component)?;
        path.push(component);
    }
    Ok(path)
}

fn safe_component(component: &str) -> Result<&str, MediaStoreError> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.contains(['/', '\\', '\0'])
        || component.chars().any(char::is_control)
    {
        return Err(MediaStoreError::InvalidPath);
    }
    Ok(component)
}

fn create_directories(root: &Path, relative_path: &Path) -> Result<(), MediaStoreError> {
    if relative_path.as_os_str().is_empty() {
        return Ok(());
    }
    let mut current = root.to_path_buf();
    for component in relative_path.components() {
        let Component::Normal(component) = component else {
            return Err(MediaStoreError::InvalidPath);
        };
        let component = component.to_str().ok_or(MediaStoreError::InvalidPath)?;
        safe_component(component)?;
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => return Err(MediaStoreError::InvalidPath),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(MediaStoreError::Publish)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;

                    fs::set_permissions(&current, fs::Permissions::from_mode(0o700))
                        .map_err(MediaStoreError::Publish)?;
                }
            }
            Err(error) => return Err(MediaStoreError::Publish(error)),
        }
    }
    Ok(())
}

fn remove_tree(path: &Path) -> Result<(), MediaStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            fs::remove_dir_all(path).map_err(MediaStoreError::Cleanup)
        }
        Ok(_) => Err(MediaStoreError::InvalidPath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MediaStoreError::Cleanup(error)),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::LiveHub;

    fn limits() -> MediaStoreLimits {
        MediaStoreLimits {
            max_bytes: 1024,
            max_files: 8,
            max_active_streams: 2,
            max_file_bytes: 512,
        }
    }

    #[test]
    fn publishes_atomically_and_rejects_stale_incarnations() {
        let root = tempdir().expect("temporary media root");
        let store = MediaStore::open(root.path().join("hls"), limits()).expect("media store");
        let hub = LiveHub::new(crate::LiveHubLimits::default());
        let key = StreamKey::new("service", "application", "stream");
        let first = hub.attach_publisher(key.clone()).expect("first publisher");
        let first_incarnation = first.incarnation();
        let first_prefix = store
            .attach(&key, first_incarnation)
            .expect("first media incarnation");
        store
            .publish(
                &key,
                first_incarnation,
                &first_prefix.join("index.m3u8"),
                b"#EXTM3U",
            )
            .expect("atomic media publish");
        assert_eq!(
            store
                .read_relative(&first_prefix.join("index.m3u8"), 64)
                .expect("published playlist"),
            b"#EXTM3U"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                std::fs::metadata(root.path().join("hls"))
                    .expect("media root permissions")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(
                    root.path()
                        .join("hls")
                        .join(&first_prefix)
                        .join("index.m3u8")
                )
                .expect("published permissions")
                .permissions()
                .mode()
                    & 0o777,
                0o600
            );
        }

        drop(first);
        let second = hub.attach_publisher(key.clone()).expect("second publisher");
        let second_prefix = store
            .attach(&key, second.incarnation())
            .expect("second media incarnation");
        assert_ne!(first_prefix, second_prefix);
        assert!(matches!(
            store.publish(
                &key,
                first_incarnation,
                &second_prefix.join("stale.ts"),
                b"stale",
            ),
            Err(MediaStoreError::StaleIncarnation)
        ));
        assert_eq!(store.stats().files, 0);
        store.close(&key, second.incarnation());
        assert_eq!(store.stats().active_streams, 0);
        assert!(store.current_prefix(&key).is_none());
    }

    #[test]
    fn enforces_file_and_storage_quotas() {
        let root = tempdir().expect("temporary media root");
        let small_limits = MediaStoreLimits {
            max_bytes: 4,
            max_files: 1,
            max_active_streams: 1,
            max_file_bytes: 4,
        };
        let store = MediaStore::open(root.path().join("hls"), small_limits).expect("media store");
        let hub = LiveHub::new(crate::LiveHubLimits::default());
        let key = StreamKey::new("service", "application", "stream");
        let lease = hub.attach_publisher(key.clone()).expect("publisher");
        let prefix = store
            .attach(&key, lease.incarnation())
            .expect("media incarnation");
        assert!(matches!(
            store.publish(
                &key,
                lease.incarnation(),
                &prefix.join("too-large.ts"),
                b"12345"
            ),
            Err(MediaStoreError::FileTooLarge)
        ));
        store
            .publish(&key, lease.incarnation(), &prefix.join("one.ts"), b"1234")
            .expect("first file");
        assert!(matches!(
            store.publish(&key, lease.incarnation(), &prefix.join("two.ts"), b"1"),
            Err(MediaStoreError::Quota)
        ));
    }

    #[test]
    fn continuing_attach_preserves_media_across_publisher_restart() {
        let root = tempdir().expect("temporary media root");
        let store = MediaStore::open(root.path().join("dash"), limits()).expect("media store");
        let hub = LiveHub::new(crate::LiveHubLimits::default());
        let key = StreamKey::new("service", "application", "stream");
        let first = hub.attach_publisher(key.clone()).expect("first publisher");
        let first_incarnation = first.incarnation();
        let first_prefix = store
            .attach_continuing(&key, first_incarnation)
            .expect("first media incarnation");
        store
            .publish(
                &key,
                first_incarnation,
                &first_prefix.join("manifest.mpd"),
                b"manifest",
            )
            .expect("manifest");
        store.close(&key, first_incarnation);
        drop(first);
        drop(store);

        let store = MediaStore::open(root.path().join("dash"), limits()).expect("reopened store");
        let second = hub.attach_publisher(key.clone()).expect("second publisher");
        let second_prefix = store
            .attach_continuing(&key, second.incarnation())
            .expect("continued media incarnation");
        assert_ne!(first_prefix, second_prefix);
        assert_eq!(
            store
                .read_relative(&second_prefix.join("manifest.mpd"), 64)
                .expect("continued manifest"),
            b"manifest"
        );
    }
}
