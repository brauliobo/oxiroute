//! Cgroup-v2 capability probing and per-worker delegated-subtree ownership.

use std::{io, path::Path};

#[cfg(target_os = "linux")]
use std::{
    fmt,
    fs::{self, File},
    io::Read as _,
    os::unix::fs::MetadataExt as _,
    path::PathBuf,
    sync::Arc,
};

/// The conventional host cgroup-v2 mount point.
pub const DEFAULT_CGROUP_V2_ROOT: &str = "/sys/fs/cgroup";

#[cfg(target_os = "linux")]
const WORKER_CGROUP_PREFIX: &str = "oxiroute-worker-";
#[cfg(target_os = "linux")]
const INSERTION_ID_BYTES: usize = 16;
#[cfg(target_os = "linux")]
const CREATE_ATTEMPTS: usize = 8;

/// Result of the cgroup-v2 capability and delegation probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CgroupV2ProbeStatus {
    /// The requested cgroup-v2 capabilities are available and delegation is observable.
    Ready,
    /// The current target is not Linux.
    Unsupported,
    /// A usable cgroup-v2 hierarchy could not be found at the probe root.
    Unavailable,
    /// The hierarchy is mounted read-only.
    ReadOnly,
    /// One or more requested controllers are not available in the hierarchy.
    MissingControllers,
    /// The hierarchy is present, but the caller cannot use the delegation surface.
    NotDelegated,
}

/// Whether the cgroup root's delegation surface can be used by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CgroupV2DelegationStatus {
    /// The root directory and required control files are writable by the caller.
    Delegated,
    /// The hierarchy is present but the required delegation writes are unavailable.
    NotDelegated,
    /// The target platform or hierarchy did not permit a delegation check.
    Unknown,
}

/// Read-only cgroup-v2 capability and delegation report.
///
/// The report contains no filesystem path. I/O failures are intentionally represented by a safe
/// status instead of an error that could disclose a host-specific path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CgroupV2Probe {
    /// Overall probe result.
    pub status: CgroupV2ProbeStatus,
    /// Controllers advertised by `cgroup.controllers`.
    pub available_controllers: Vec<String>,
    /// Requested controllers absent from `cgroup.controllers`.
    pub missing_controllers: Vec<String>,
    /// Whether the cgroup root exposes the required delegation writes.
    pub delegation: CgroupV2DelegationStatus,
}

impl CgroupV2Probe {
    /// Returns whether the requested capability and delegation checks succeeded.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self.status, CgroupV2ProbeStatus::Ready)
    }
}

/// Probes the conventional host cgroup-v2 mount without changing it.
#[must_use]
pub fn probe_cgroup_v2() -> CgroupV2Probe {
    probe_cgroup_v2_at_with_controllers(Path::new(DEFAULT_CGROUP_V2_ROOT), &[])
}

/// Probes an injected cgroup-v2 root without changing it.
///
/// The root is injected so callers and tests can select the hierarchy explicitly. No process
/// discovery or `/proc` lookup is performed.
#[must_use]
pub fn probe_cgroup_v2_at(root: impl AsRef<Path>) -> CgroupV2Probe {
    probe_cgroup_v2_at_with_controllers(root, &[])
}

/// Probes an injected cgroup-v2 root and checks the requested controllers.
///
/// Controller names are compared as whitespace-delimited cgroup-v2 names. Duplicate or empty
/// requested names do not change the result.
#[must_use]
pub fn probe_cgroup_v2_at_with_controllers(
    root: impl AsRef<Path>,
    required_controllers: &[&str],
) -> CgroupV2Probe {
    #[cfg(target_os = "linux")]
    {
        probe_linux(root.as_ref(), required_controllers)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (root, required_controllers);
        unsupported_probe()
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[cfg(target_os = "linux")]
trait CgroupFileSystem: fmt::Debug + Send + Sync {
    fn create_dir(&self, path: &Path) -> io::Result<DirectoryIdentity>;
    fn identity(&self, path: &Path) -> io::Result<DirectoryIdentity>;
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()>;
    fn remove_dir(&self, path: &Path) -> io::Result<()>;
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct SystemCgroupFileSystem;

#[cfg(target_os = "linux")]
impl CgroupFileSystem for SystemCgroupFileSystem {
    fn create_dir(&self, path: &Path) -> io::Result<DirectoryIdentity> {
        fs::create_dir(path)?;
        self.identity(path)
    }

    fn identity(&self, path: &Path) -> io::Result<DirectoryIdentity> {
        let metadata = fs::metadata(path)?;
        Ok(DirectoryIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }

    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        fs::write(path, contents)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir(path)
    }
}

/// Exclusive ownership of one per-worker cgroup insertion.
///
/// The fixed-size random insertion id makes names bounded and avoids reusing stale directories.
/// Cleanup additionally compares the directory identity captured at insertion, so a stale owner
/// cannot kill or remove a replacement that has appeared at the same path.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct WorkerCgroupLease {
    path: PathBuf,
    identity: DirectoryIdentity,
    filesystem: Arc<dyn CgroupFileSystem>,
    released: bool,
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
pub(crate) struct WorkerCgroupLease;

impl WorkerCgroupLease {
    pub(crate) fn try_create(root: &Path) -> io::Result<Option<Self>> {
        #[cfg(target_os = "linux")]
        {
            if !probe_cgroup_v2_at(root).is_ready() {
                return Ok(None);
            }
            let filesystem: Arc<dyn CgroupFileSystem> = Arc::new(SystemCgroupFileSystem);
            for _ in 0..CREATE_ATTEMPTS {
                let insertion_id = random_insertion_id()?;
                match Self::create_with_id(root, insertion_id, Arc::clone(&filesystem)) {
                    Ok(lease) => return Ok(Some(lease)),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not allocate a unique worker cgroup insertion",
            ))
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = root;
            Ok(None)
        }
    }

    #[cfg(target_os = "linux")]
    fn create_with_id(
        root: &Path,
        insertion_id: [u8; INSERTION_ID_BYTES],
        filesystem: Arc<dyn CgroupFileSystem>,
    ) -> io::Result<Self> {
        let path = root.join(worker_cgroup_name(insertion_id));
        let identity = filesystem.create_dir(&path)?;
        Ok(Self {
            path,
            identity,
            filesystem,
            released: false,
        })
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn path(&self) -> &Path {
        unreachable!("non-Linux workers never acquire a cgroup lease")
    }

    pub(crate) fn kill(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            if self.still_owns_path()? {
                self.filesystem
                    .write(&self.path.join("cgroup.kill"), b"1")?;
            }
        }
        Ok(())
    }

    /// Removes an empty cgroup if this lease still owns its insertion.
    ///
    /// `Ok(false)` means the cgroup remains populated and cleanup should be retried by the reaper.
    pub(crate) fn cleanup(&mut self) -> io::Result<bool> {
        #[cfg(target_os = "linux")]
        {
            if self.released {
                return Ok(true);
            }
            if !self.still_owns_path()? {
                self.released = true;
                return Ok(true);
            }
            let events = self
                .filesystem
                .read_to_string(&self.path.join("cgroup.events"))?;
            if cgroup_is_populated(&events)? {
                return Ok(false);
            }
            if !self.still_owns_path()? {
                self.released = true;
                return Ok(true);
            }
            match self.filesystem.remove_dir(&self.path) {
                Ok(()) => {
                    self.released = true;
                    Ok(true)
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    self.released = true;
                    Ok(true)
                }
                Err(error) => Err(error),
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            Ok(true)
        }
    }

    #[cfg(target_os = "linux")]
    fn still_owns_path(&self) -> io::Result<bool> {
        match self.filesystem.identity(&self.path) {
            Ok(identity) => Ok(identity == self.identity),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

impl Drop for WorkerCgroupLease {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

/// Attaches the current launcher to a cgroup before it creates the worker.
#[cfg(target_os = "linux")]
pub(crate) fn attach_current(path: &Path) -> io::Result<()> {
    fs::write(path.join("cgroup.procs"), std::process::id().to_string())
}

/// Kills all processes in the launcher's cgroup. A successful real cgroup write does not return.
#[cfg(target_os = "linux")]
pub(crate) fn kill_at(path: &Path) -> io::Result<()> {
    fs::write(path.join("cgroup.kill"), "1")
}

#[cfg(target_os = "linux")]
fn random_insertion_id() -> io::Result<[u8; INSERTION_ID_BYTES]> {
    let mut insertion_id = [0_u8; INSERTION_ID_BYTES];
    File::open("/dev/urandom")?.read_exact(&mut insertion_id)?;
    Ok(insertion_id)
}

#[cfg(target_os = "linux")]
fn worker_cgroup_name(insertion_id: [u8; INSERTION_ID_BYTES]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut name = String::with_capacity(WORKER_CGROUP_PREFIX.len() + INSERTION_ID_BYTES * 2);
    name.push_str(WORKER_CGROUP_PREFIX);
    for byte in insertion_id {
        name.push(char::from(DIGITS[usize::from(byte >> 4)]));
        name.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    name
}

#[cfg(target_os = "linux")]
fn cgroup_is_populated(events: &str) -> io::Result<bool> {
    events
        .lines()
        .find_map(|line| line.strip_prefix("populated "))
        .and_then(|value| match value {
            "0" => Some(false),
            "1" => Some(true),
            _ => None,
        })
        .ok_or_else(|| io::Error::other("cgroup.events omitted a valid populated state"))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
    };

    use super::{
        CgroupFileSystem, DirectoryIdentity, WorkerCgroupLease, controller_names,
        worker_cgroup_name,
    };

    #[derive(Debug, Default)]
    struct FakeCgroupFileSystem {
        state: Mutex<FakeState>,
    }

    #[derive(Debug, Default)]
    struct FakeState {
        next_inode: u64,
        directories: HashMap<PathBuf, FakeDirectory>,
        removed: Vec<PathBuf>,
    }

    #[derive(Debug)]
    struct FakeDirectory {
        identity: DirectoryIdentity,
        populated: bool,
        writes: Vec<(String, Vec<u8>)>,
    }

    impl FakeCgroupFileSystem {
        fn set_populated(&self, path: &Path, populated: bool) {
            self.state
                .lock()
                .unwrap()
                .directories
                .get_mut(path)
                .expect("fake cgroup")
                .populated = populated;
        }

        fn replace(&self, path: &Path) {
            let mut state = self.state.lock().unwrap();
            state.next_inode += 1;
            let identity = DirectoryIdentity {
                device: 1,
                inode: state.next_inode,
            };
            state.directories.insert(
                path.to_owned(),
                FakeDirectory {
                    identity,
                    populated: false,
                    writes: Vec::new(),
                },
            );
        }

        fn contains(&self, path: &Path) -> bool {
            self.state.lock().unwrap().directories.contains_key(path)
        }

        fn writes(&self, path: &Path) -> Vec<(String, Vec<u8>)> {
            self.state
                .lock()
                .unwrap()
                .directories
                .get(path)
                .expect("fake cgroup")
                .writes
                .clone()
        }
    }

    impl CgroupFileSystem for FakeCgroupFileSystem {
        fn create_dir(&self, path: &Path) -> std::io::Result<DirectoryIdentity> {
            let mut state = self.state.lock().unwrap();
            if state.directories.contains_key(path) {
                return Err(std::io::ErrorKind::AlreadyExists.into());
            }
            state.next_inode += 1;
            let identity = DirectoryIdentity {
                device: 1,
                inode: state.next_inode,
            };
            state.directories.insert(
                path.to_owned(),
                FakeDirectory {
                    identity,
                    populated: false,
                    writes: Vec::new(),
                },
            );
            Ok(identity)
        }

        fn identity(&self, path: &Path) -> std::io::Result<DirectoryIdentity> {
            self.state
                .lock()
                .unwrap()
                .directories
                .get(path)
                .map(|directory| directory.identity)
                .ok_or_else(|| std::io::ErrorKind::NotFound.into())
        }

        fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
            let parent = path.parent().ok_or(std::io::ErrorKind::NotFound)?;
            let state = self.state.lock().unwrap();
            let directory = state
                .directories
                .get(parent)
                .ok_or(std::io::ErrorKind::NotFound)?;
            Ok(format!("populated {}\n", u8::from(directory.populated)))
        }

        fn write(&self, path: &Path, contents: &[u8]) -> std::io::Result<()> {
            let parent = path.parent().ok_or(std::io::ErrorKind::NotFound)?;
            let mut state = self.state.lock().unwrap();
            let directory = state
                .directories
                .get_mut(parent)
                .ok_or(std::io::ErrorKind::NotFound)?;
            directory.writes.push((
                path.file_name()
                    .and_then(|name| name.to_str())
                    .ok_or(std::io::ErrorKind::InvalidInput)?
                    .to_owned(),
                contents.to_vec(),
            ));
            Ok(())
        }

        fn remove_dir(&self, path: &Path) -> std::io::Result<()> {
            let mut state = self.state.lock().unwrap();
            state
                .directories
                .remove(path)
                .ok_or(std::io::ErrorKind::NotFound)?;
            state.removed.push(path.to_owned());
            Ok(())
        }
    }

    #[test]
    fn controller_names_are_sorted_and_deduplicated() {
        assert_eq!(
            controller_names("memory cpu memory\n"),
            ["cpu".to_owned(), "memory".to_owned()]
        );
    }

    #[test]
    fn worker_names_are_fixed_bounded_insertion_identifiers() {
        let name = worker_cgroup_name([0xab; 16]);
        assert_eq!(name, "oxiroute-worker-abababababababababababababababab");
        assert_eq!(name.len(), 48);
    }

    #[test]
    fn fake_filesystem_lease_kills_and_removes_only_after_empty() {
        let filesystem = Arc::new(FakeCgroupFileSystem::default());
        let filesystem_trait: Arc<dyn CgroupFileSystem> = filesystem.clone();
        let mut lease =
            WorkerCgroupLease::create_with_id(Path::new("/delegated"), [1; 16], filesystem_trait)
                .unwrap();
        let path = lease.path().to_owned();
        filesystem.set_populated(&path, true);

        lease.kill().unwrap();
        assert!(!lease.cleanup().unwrap());
        assert_eq!(
            filesystem.writes(&path),
            [("cgroup.kill".into(), b"1".to_vec())]
        );

        filesystem.set_populated(&path, false);
        assert!(lease.cleanup().unwrap());
        assert!(!filesystem.contains(&path));
    }

    #[test]
    fn stale_fake_filesystem_owner_preserves_replacement() {
        let filesystem = Arc::new(FakeCgroupFileSystem::default());
        let filesystem_trait: Arc<dyn CgroupFileSystem> = filesystem.clone();
        let mut lease =
            WorkerCgroupLease::create_with_id(Path::new("/delegated"), [2; 16], filesystem_trait)
                .unwrap();
        let path = lease.path().to_owned();
        filesystem.replace(&path);

        lease.kill().unwrap();
        assert!(lease.cleanup().unwrap());
        assert!(filesystem.contains(&path));
        assert!(filesystem.writes(&path).is_empty());
    }
}

#[cfg(target_os = "linux")]
fn probe_linux(root: &Path, required_controllers: &[&str]) -> CgroupV2Probe {
    let controllers_path = root.join("cgroup.controllers");
    let subtree_control_path = root.join("cgroup.subtree_control");
    let processes_path = root.join("cgroup.procs");

    if !root.is_dir()
        || !is_file(&controllers_path)
        || !is_file(&subtree_control_path)
        || !is_file(&processes_path)
    {
        return unavailable_probe();
    }

    let Ok(controllers) = fs::read_to_string(&controllers_path) else {
        return unavailable_probe();
    };
    let available_controllers = controller_names(&controllers);
    let missing_controllers =
        missing_controller_names(&available_controllers, required_controllers);
    let delegation =
        if writable_directory(root) && writable(&subtree_control_path) && writable(&processes_path)
        {
            CgroupV2DelegationStatus::Delegated
        } else {
            CgroupV2DelegationStatus::NotDelegated
        };

    let status = if read_only(root, &subtree_control_path, &processes_path) {
        CgroupV2ProbeStatus::ReadOnly
    } else if !missing_controllers.is_empty() {
        CgroupV2ProbeStatus::MissingControllers
    } else if matches!(delegation, CgroupV2DelegationStatus::NotDelegated) {
        CgroupV2ProbeStatus::NotDelegated
    } else {
        CgroupV2ProbeStatus::Ready
    };

    CgroupV2Probe {
        status,
        available_controllers,
        missing_controllers,
        delegation,
    }
}

#[cfg(target_os = "linux")]
fn is_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

#[cfg(target_os = "linux")]
fn controller_names(contents: &str) -> Vec<String> {
    let mut controllers = contents
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    controllers.sort_unstable();
    controllers.dedup();
    controllers
}

#[cfg(target_os = "linux")]
fn missing_controller_names(available: &[String], required: &[&str]) -> Vec<String> {
    let mut missing = required
        .iter()
        .map(|controller| controller.trim())
        .filter(|controller| !controller.is_empty())
        .filter(|controller| !available.iter().any(|available| available == controller))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    missing.sort_unstable();
    missing.dedup();
    missing
}

#[cfg(target_os = "linux")]
fn writable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| {
        !metadata.permissions().readonly()
            && rustix::fs::access(path, rustix::fs::Access::WRITE_OK).is_ok()
    })
}

#[cfg(target_os = "linux")]
fn writable_directory(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| {
        metadata.is_dir()
            && !metadata.permissions().readonly()
            && rustix::fs::access(
                path,
                rustix::fs::Access::WRITE_OK | rustix::fs::Access::EXEC_OK,
            )
            .is_ok()
    })
}

#[cfg(target_os = "linux")]
fn read_only(root: &Path, subtree_control: &Path, processes: &Path) -> bool {
    cgroup_mount_is_read_only(root)
        || [root, subtree_control, processes]
            .into_iter()
            .all(|path| fs::metadata(path).is_ok_and(|metadata| metadata.permissions().readonly()))
}

#[cfg(target_os = "linux")]
fn cgroup_mount_is_read_only(root: &Path) -> bool {
    const CGROUP2_SUPER_MAGIC: u64 = 0x6367_7270;
    const MS_RDONLY: u64 = 1;

    rustix::fs::statfs(root).is_ok_and(|stat| {
        u64::try_from(stat.f_type).is_ok_and(|filesystem_type| {
            u64::try_from(stat.f_flags)
                .is_ok_and(|flags| filesystem_type == CGROUP2_SUPER_MAGIC && flags & MS_RDONLY != 0)
        })
    })
}

#[cfg(target_os = "linux")]
fn unavailable_probe() -> CgroupV2Probe {
    CgroupV2Probe {
        status: CgroupV2ProbeStatus::Unavailable,
        available_controllers: Vec::new(),
        missing_controllers: Vec::new(),
        delegation: CgroupV2DelegationStatus::Unknown,
    }
}

#[cfg(not(target_os = "linux"))]
fn unsupported_probe() -> CgroupV2Probe {
    CgroupV2Probe {
        status: CgroupV2ProbeStatus::Unsupported,
        available_controllers: Vec::new(),
        missing_controllers: Vec::new(),
        delegation: CgroupV2DelegationStatus::Unknown,
    }
}
