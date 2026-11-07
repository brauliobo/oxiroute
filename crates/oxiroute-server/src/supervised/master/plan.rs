#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConfigPlan {
    Supervised,
    Direct,
}

#[derive(Debug, Eq, Error, PartialEq)]
#[error("supervised master does not support this configuration: {reason}")]
pub(super) struct UnsupportedConfig {
    pub(super) reason: &'static str,
}

pub(super) fn config_plan(
    config: &ValidatedConfig,
    launcher_available: bool,
    containment_status: CgroupV2ProbeStatus,
) -> Result<ConfigPlan, UnsupportedConfig> {
    let contains_exec = contains_rtmp_exec_profiles(config);
    if let Err(error) = supervised_eligibility(config, containment_status) {
        return if contains_exec {
            Err(error)
        } else {
            Ok(ConfigPlan::Direct)
        };
    }
    if launcher_available {
        Ok(ConfigPlan::Supervised)
    } else if contains_exec {
        Err(UnsupportedConfig {
            reason: "RTMP exec profiles require the supervised launcher",
        })
    } else {
        Ok(ConfigPlan::Direct)
    }
}

pub(super) fn supervised_eligibility(
    config: &ValidatedConfig,
    containment_status: CgroupV2ProbeStatus,
) -> Result<(), UnsupportedConfig> {
    worker::validate_stage_one_config(config).map_err(|reason| UnsupportedConfig { reason })?;
    if contains_rtmp_exec_profiles(config) && containment_status != CgroupV2ProbeStatus::Ready {
        return Err(UnsupportedConfig {
            reason: containment_error_reason(containment_status),
        });
    }
    Ok(())
}

pub(super) fn contains_rtmp_exec_profiles(config: &ValidatedConfig) -> bool {
    config
        .as_draft()
        .rtmp_services
        .iter()
        .any(|service| !service.exec_profiles.is_empty())
}

const fn containment_error_reason(status: CgroupV2ProbeStatus) -> &'static str {
    match status {
        CgroupV2ProbeStatus::Ready => "RTMP exec profile containment is ready",
        CgroupV2ProbeStatus::Unsupported => {
            "RTMP exec profiles require Linux cgroup-v2 containment"
        }
        CgroupV2ProbeStatus::Unavailable => {
            "RTMP exec profiles require an available cgroup-v2 hierarchy"
        }
        CgroupV2ProbeStatus::ReadOnly => "RTMP exec profiles require writable cgroup-v2 delegation",
        CgroupV2ProbeStatus::MissingControllers => {
            "RTMP exec profiles require the configured cgroup-v2 controllers"
        }
        CgroupV2ProbeStatus::NotDelegated => {
            "RTMP exec profiles require delegated cgroup-v2 containment"
        }
    }
}

pub(super) fn load_document(
    coordinator: &CanonicalConfigCoordinator,
) -> Result<Box<ResolvedConfigDocument>, Box<dyn Error>> {
    match coordinator.load() {
        ConfigLoadOutcome::Loaded(document) => Ok(document),
        ConfigLoadOutcome::Rejected(rejection) => {
            Err(io::Error::new(io::ErrorKind::InvalidData, rejection.message()).into())
        }
    }
}
