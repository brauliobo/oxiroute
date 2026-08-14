#[cfg(target_os = "linux")]
use std::fs;

#[cfg(target_os = "linux")]
use oxiroute_supervisor_process::cgroup::probe_cgroup_v2_at_with_controllers;
use oxiroute_supervisor_process::cgroup::{
    CgroupV2DelegationStatus, CgroupV2ProbeStatus, probe_cgroup_v2_at,
};
#[cfg(target_os = "linux")]
use tempfile::TempDir;

#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;

#[cfg(target_os = "linux")]
struct CgroupFixture {
    directory: TempDir,
}

#[cfg(target_os = "linux")]
impl CgroupFixture {
    fn new(controllers: &str) -> Self {
        let directory = tempfile::tempdir().expect("fixture directory");
        fs::write(directory.path().join("cgroup.controllers"), controllers).expect("controllers");
        fs::write(directory.path().join("cgroup.subtree_control"), "").expect("subtree control");
        fs::write(directory.path().join("cgroup.procs"), "").expect("process list");

        for name in [
            "cgroup.controllers",
            "cgroup.subtree_control",
            "cgroup.procs",
        ] {
            fs::set_permissions(
                directory.path().join(name),
                fs::Permissions::from_mode(0o644),
            )
            .expect("fixture file permissions");
        }
        Self { directory }
    }

    fn path(&self) -> &std::path::Path {
        self.directory.path()
    }

    fn set_mode(&self, mode: u32) {
        fs::set_permissions(self.path(), fs::Permissions::from_mode(mode))
            .expect("fixture directory permissions");
    }

    fn set_file_mode(&self, name: &str, mode: u32) {
        fs::set_permissions(self.path().join(name), fs::Permissions::from_mode(mode))
            .expect("fixture file permissions");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn delegated_v2_fixture_reports_available_controllers() {
    let fixture = CgroupFixture::new("cpu memory pids\n");
    fs::write(
        fixture.path().join("cgroup.subtree_control"),
        "cpu memory pids\n",
    )
    .expect("subtree control contents");
    fs::write(fixture.path().join("cgroup.procs"), "123\n").expect("process list contents");

    let probe = probe_cgroup_v2_at_with_controllers(fixture.path(), &["pids", "memory"]);

    assert_eq!(probe.status, CgroupV2ProbeStatus::Ready);
    assert_eq!(probe.delegation, CgroupV2DelegationStatus::Delegated);
    assert_eq!(probe.available_controllers, ["cpu", "memory", "pids"]);
    assert!(probe.missing_controllers.is_empty());
    assert_eq!(
        fs::read_to_string(fixture.path().join("cgroup.subtree_control")).unwrap(),
        "cpu memory pids\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.path().join("cgroup.procs")).unwrap(),
        "123\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn missing_controller_is_distinct_from_delegation() {
    let fixture = CgroupFixture::new("cpu pids\n");

    let probe = probe_cgroup_v2_at_with_controllers(fixture.path(), &["memory"]);

    assert_eq!(probe.status, CgroupV2ProbeStatus::MissingControllers);
    assert_eq!(probe.delegation, CgroupV2DelegationStatus::Delegated);
    assert_eq!(probe.missing_controllers, ["memory"]);
}

#[cfg(target_os = "linux")]
#[test]
fn missing_control_file_is_unavailable() {
    let fixture = CgroupFixture::new("pids\n");
    fs::remove_file(fixture.path().join("cgroup.procs")).expect("remove control file");

    let probe = probe_cgroup_v2_at(fixture.path());

    assert_eq!(probe.status, CgroupV2ProbeStatus::Unavailable);
    assert_eq!(probe.delegation, CgroupV2DelegationStatus::Unknown);
}

#[cfg(target_os = "linux")]
#[test]
fn read_only_fixture_is_distinct_from_undelegated_fixture() {
    let read_only = CgroupFixture::new("pids\n");
    read_only.set_mode(0o555);
    for name in [
        "cgroup.controllers",
        "cgroup.subtree_control",
        "cgroup.procs",
    ] {
        read_only.set_file_mode(name, 0o444);
    }
    let read_only_probe = probe_cgroup_v2_at(read_only.path());
    assert_eq!(read_only_probe.status, CgroupV2ProbeStatus::ReadOnly);
    assert_eq!(
        read_only_probe.delegation,
        CgroupV2DelegationStatus::NotDelegated
    );
    read_only.set_mode(0o700);

    let undelegated = CgroupFixture::new("pids\n");
    undelegated.set_file_mode("cgroup.subtree_control", 0o444);
    undelegated.set_file_mode("cgroup.procs", 0o444);
    let undelegated_probe = probe_cgroup_v2_at(undelegated.path());
    assert_eq!(undelegated_probe.status, CgroupV2ProbeStatus::NotDelegated);
    assert_eq!(
        undelegated_probe.delegation,
        CgroupV2DelegationStatus::NotDelegated
    );
}

#[cfg(target_os = "linux")]
#[test]
fn missing_v2_root_is_unavailable_without_exposing_a_path() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let missing = directory.path().join("not-a-cgroup");

    let probe = probe_cgroup_v2_at(&missing);

    assert_eq!(probe.status, CgroupV2ProbeStatus::Unavailable);
    assert_eq!(probe.delegation, CgroupV2DelegationStatus::Unknown);
    let debug = format!("{probe:?}");
    assert!(!debug.contains("not-a-cgroup"));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn non_linux_probe_is_safe_and_unsupported() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let probe = probe_cgroup_v2_at(directory.path());

    assert_eq!(probe.status, CgroupV2ProbeStatus::Unsupported);
    assert_eq!(probe.delegation, CgroupV2DelegationStatus::Unknown);
}
