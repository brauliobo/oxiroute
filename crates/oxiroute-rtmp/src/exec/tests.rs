
use std::path::PathBuf;

use super::*;

fn limits() -> ExecLimits {
    ExecLimits::new(
        8,
        64 * 1024,
        64 * 1024,
        64 * 1024,
        Duration::from_secs(5),
        Duration::from_secs(1),
        2,
        Duration::from_millis(10),
        2,
    )
    .expect("test limits are valid")
}

fn profile(
    executable: &str,
    mode: ExecMode,
    network: ExecNetworkPolicy,
    environment: Vec<ExecEnvironment>,
) -> ExecProfile {
    ExecProfile::new(
        "capture",
        "live",
        mode,
        ExecTrigger::Publisher,
        PathBuf::from(executable),
        Vec::<String>::new(),
        environment,
        PathBuf::from("/tmp"),
        ExecFilesystemPolicy::WorkingDirectory,
        network,
        limits(),
        false,
    )
    .expect("test profile is valid")
}

#[test]
fn accepts_bounded_absolute_paths() {
    let profile = profile(
        "/usr/bin/cat",
        ExecMode::Command,
        ExecNetworkPolicy::Inherited,
        vec![ExecEnvironment::new("CAPTURE_MODE", "raw").unwrap()],
    );
    assert_eq!(profile.executable(), Path::new("/usr/bin/cat"));
    assert_eq!(profile.working_directory(), Path::new("/tmp"));
}

#[test]
fn rejects_traversal_and_shell_paths() {
    assert_eq!(
        ExecProfile::new(
            "capture",
            "live",
            ExecMode::Command,
            ExecTrigger::Publisher,
            PathBuf::from("/usr/bin/../cat"),
            Vec::<String>::new(),
            Vec::<ExecEnvironment>::new(),
            PathBuf::from("/tmp"),
            ExecFilesystemPolicy::WorkingDirectory,
            ExecNetworkPolicy::Inherited,
            limits(),
            false,
        )
        .unwrap_err(),
        ExecProfileError::InvalidExecutable
    );
    assert_eq!(
        ExecProfile::new(
            "capture",
            "live",
            ExecMode::Command,
            ExecTrigger::Publisher,
            PathBuf::from("/bin/sh"),
            Vec::<String>::new(),
            Vec::<ExecEnvironment>::new(),
            PathBuf::from("/tmp"),
            ExecFilesystemPolicy::WorkingDirectory,
            ExecNetworkPolicy::Inherited,
            limits(),
            false,
        )
        .unwrap_err(),
        ExecProfileError::ShellExecutable
    );
}

#[test]
fn rejects_duplicate_environment_names() {
    let environment = ExecEnvironment::new("CAPTURE_MODE", "raw").unwrap();
    let error = ExecProfile::new(
        "capture",
        "live",
        ExecMode::Command,
        ExecTrigger::Publisher,
        PathBuf::from("/usr/bin/cat"),
        Vec::<String>::new(),
        vec![environment.clone(), environment],
        PathBuf::from("/tmp"),
        ExecFilesystemPolicy::WorkingDirectory,
        ExecNetworkPolicy::Inherited,
        limits(),
        false,
    )
    .unwrap_err();
    assert_eq!(error, ExecProfileError::InvalidEnvironment);
}

#[test]
fn debug_output_redacts_process_inputs() {
    let profile = ExecProfile::new(
        "capture",
        "live",
        ExecMode::Command,
        ExecTrigger::Publisher,
        PathBuf::from("/usr/bin/cat"),
        vec!["secret-argument".into()],
        vec![ExecEnvironment::new("CAPTURE_MODE", "secret-value").unwrap()],
        PathBuf::from("/tmp"),
        ExecFilesystemPolicy::WorkingDirectory,
        ExecNetworkPolicy::Inherited,
        limits(),
        false,
    )
    .unwrap();
    let debug = format!("{profile:?}");
    assert!(!debug.contains("/usr/bin/cat"));
    assert!(!debug.contains("secret-argument"));
    assert!(!debug.contains("secret-value"));
}
