//! Read-only cgroup-v2 capability and delegation probing.
//!
//! The probe only reads the supplied cgroup directory and its standard control files. It never
//! creates a cgroup, writes controller state, moves a process, or scans `/proc`.

use std::path::Path;

#[cfg(target_os = "linux")]
use std::fs;

/// The conventional host cgroup-v2 mount point.
pub const DEFAULT_CGROUP_V2_ROOT: &str = "/sys/fs/cgroup";

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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::controller_names;

    #[test]
    fn controller_names_are_sorted_and_deduplicated() {
        assert_eq!(
            controller_names("memory cpu memory\n"),
            ["cpu".to_owned(), "memory".to_owned()]
        );
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
