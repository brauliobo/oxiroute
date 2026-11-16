use oxiroute_config::{ConfigDraft, HttpDataPlane, ListenerBind, Protocol};

use super::{HttpServicePlan, ServicePlanError};
use crate::tls::TlsProfilePlan;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataPlane {
    Tokio,
    Epoll,
    IoUring,
}

impl DataPlane {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tokio => "tokio",
            Self::Epoll => "epoll",
            Self::IoUring => "io_uring",
        }
    }

    pub(crate) fn plan(
        listener: ListenerPlane,
        service: &HttpServicePlan,
        tls: Option<&TlsProfilePlan>,
    ) -> Self {
        let terminable = tls.is_none_or(TlsProfilePlan::serves_on_epoll);
        match listener.engine {
            Some(engine) if terminable && service.serves_on_engine() => engine,
            _ => Self::Tokio,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ListenerPlane {
    engine: Option<DataPlane>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct EngineConditions {
    pub(crate) supported: bool,
    pub(crate) fast_lane: bool,
    pub(crate) per_worker_scheduling: bool,
}

impl EngineConditions {
    fn of(config: &ConfigDraft) -> Self {
        Self {
            supported: oxiroute_epoll_h1::supported(),
            fast_lane: config.fast_lane_enabled(),
            per_worker_scheduling: config.aged_per_worker(),
        }
    }

    fn met(self) -> bool {
        self.supported && self.fast_lane
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DataPlanePolicy {
    setting: Option<HttpDataPlane>,
    conditions: EngineConditions,
}

#[cfg(target_os = "linux")]
fn io_uring_unavailable() -> Option<String> {
    oxiroute_epoll_h1::io_uring_support()
        .err()
        .map(|missing| missing.to_string())
}

#[cfg(not(target_os = "linux"))]
fn io_uring_unavailable() -> Option<String> {
    Some("only Linux provides io_uring".to_owned())
}

impl DataPlanePolicy {
    pub(crate) fn of(config: &ConfigDraft) -> Result<Self, ServicePlanError> {
        Self::resolve(
            config.http_data_plane_choice(),
            EngineConditions::of(config),
            io_uring_unavailable,
        )
    }

    pub(crate) fn resolve(
        setting: Option<HttpDataPlane>,
        conditions: EngineConditions,
        io_uring_unavailable: impl FnOnce() -> Option<String>,
    ) -> Result<Self, ServicePlanError> {
        match setting {
            Some(HttpDataPlane::Epoll) if !conditions.supported => {
                Err(ServicePlanError::EpollUnsupported)
            }
            Some(HttpDataPlane::Epoll) if !conditions.fast_lane => {
                Err(ServicePlanError::EpollNeedsFastLane)
            }
            Some(HttpDataPlane::IoUring) if !conditions.fast_lane => {
                Err(ServicePlanError::IoUringNeedsFastLane)
            }
            Some(HttpDataPlane::IoUring) if conditions.per_worker_scheduling => {
                Err(ServicePlanError::IoUringNeedsSharedScheduling)
            }
            Some(HttpDataPlane::IoUring) => match io_uring_unavailable() {
                Some(missing) => Err(ServicePlanError::IoUringUnsupported { missing }),
                None => Ok(Self::new(setting, conditions)),
            },
            _ => Ok(Self::new(setting, conditions)),
        }
    }

    const fn new(setting: Option<HttpDataPlane>, conditions: EngineConditions) -> Self {
        Self {
            setting,
            conditions,
        }
    }

    const fn engine(self) -> DataPlane {
        match self.setting {
            Some(HttpDataPlane::IoUring) => DataPlane::IoUring,
            _ => DataPlane::Epoll,
        }
    }

    pub(crate) fn listener(
        self,
        protocol: Protocol,
        bind: &ListenerBind,
        tls: bool,
    ) -> ListenerPlane {
        let on_engine = self.setting != Some(HttpDataPlane::Tokio)
            && self.conditions.met()
            && protocol == Protocol::Http
            && (!tls || self.setting == Some(HttpDataPlane::Epoll))
            && matches!(bind, ListenerBind::Socket { .. });
        ListenerPlane {
            engine: on_engine.then(|| self.engine()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::*;

    fn socket() -> ListenerBind {
        ListenerBind::Socket {
            address: SocketAddr::from(([127, 0, 0, 1], 8080)),
            ipv6_only: false,
        }
    }

    const MET: EngineConditions = EngineConditions {
        supported: true,
        fast_lane: true,
        per_worker_scheduling: false,
    };

    fn available() -> Option<String> {
        None
    }

    fn policy(setting: Option<HttpDataPlane>, conditions: EngineConditions) -> DataPlanePolicy {
        DataPlanePolicy::resolve(setting, conditions, available).expect("a valid policy")
    }

    fn engine(plane: ListenerPlane) -> Option<DataPlane> {
        plane.engine
    }

    #[test]
    fn plain_http_listeners_default_to_epoll_where_it_is_supported() {
        let plane = policy(None, MET).listener(Protocol::Http, &socket(), false);
        assert_eq!(engine(plane), Some(DataPlane::Epoll));
    }

    #[test]
    fn nothing_selects_epoll_where_it_is_not_supported() {
        let conditions = EngineConditions {
            supported: false,
            ..MET
        };
        let plane = policy(None, conditions).listener(Protocol::Http, &socket(), false);
        assert_eq!(engine(plane), None);
    }

    #[test]
    fn tokio_forces_the_tokio_path_everywhere() {
        let plane =
            policy(Some(HttpDataPlane::Tokio), MET).listener(Protocol::Http, &socket(), false);
        assert_eq!(engine(plane), None);
    }

    #[test]
    fn tls_listeners_use_epoll_only_when_it_is_chosen_explicitly() {
        let explicit = policy(Some(HttpDataPlane::Epoll), MET);
        let on_tls = |policy: DataPlanePolicy| policy.listener(Protocol::Http, &socket(), true);
        assert_eq!(engine(on_tls(explicit)), Some(DataPlane::Epoll));
        assert_eq!(engine(on_tls(policy(None, MET))), None);
    }

    #[test]
    fn http3_and_unix_listeners_stay_on_the_tokio_path() {
        let policy = policy(Some(HttpDataPlane::Epoll), MET);
        assert_eq!(
            engine(policy.listener(Protocol::Http3, &socket(), false)),
            None
        );
        let unix = ListenerBind::Unix {
            path: "/run/example.sock".into(),
            mode: None,
        };
        assert_eq!(engine(policy.listener(Protocol::Http, &unix, false)), None);
    }

    #[test]
    fn the_fast_lane_off_keeps_the_default_on_the_tokio_path() {
        let conditions = EngineConditions {
            fast_lane: false,
            ..MET
        };
        let plane = policy(None, conditions).listener(Protocol::Http, &socket(), false);
        assert_eq!(engine(plane), None);
    }

    #[test]
    fn choosing_epoll_explicitly_is_refused_where_it_cannot_run() {
        let epoll = Some(HttpDataPlane::Epoll);
        let refused = |conditions| DataPlanePolicy::resolve(epoll, conditions, available);
        assert!(matches!(
            refused(EngineConditions {
                supported: false,
                ..MET
            }),
            Err(ServicePlanError::EpollUnsupported)
        ));
        assert!(matches!(
            refused(EngineConditions {
                fast_lane: false,
                ..MET
            }),
            Err(ServicePlanError::EpollNeedsFastLane)
        ));
    }

    fn scheduled(plane: Option<&str>) -> ConfigDraft {
        let plane = plane.map_or_else(String::new, |plane| {
            format!(r#""http_data_plane":"{plane}","#)
        });
        let draft = format!(
            r#"{{{plane}"version":1,"connection_scheduling":"aged_per_worker","worker_scaling":"dynamic",
            "connection_age_ms":5,"min_active_workers":1,"target_busy_percent":70,"scale_down_delay_ms":200,
            "certificates":[],"listeners":[]}}"#
        );
        serde_json::from_str(&draft).expect("a scheduled draft")
    }

    #[test]
    fn aged_scheduling_and_dynamic_scaling_do_not_leave_the_epoll_plane() {
        for plane in [None, Some("epoll")] {
            let config = scheduled(plane);
            assert!(config.aged_per_worker() && config.dynamic_worker_scaling());
            let policy = DataPlanePolicy::resolve(
                config.http_data_plane_choice(),
                EngineConditions::of(&config),
                available,
            )
            .expect("scheduling is not a condition of the epoll driver");
            let plane = policy.listener(Protocol::Http, &socket(), false);
            assert_eq!(engine(plane), Some(DataPlane::Epoll));
        }
    }

    #[test]
    fn choosing_tokio_is_accepted_everywhere() {
        let nothing = EngineConditions {
            supported: false,
            fast_lane: false,
            per_worker_scheduling: true,
        };
        let tokio = Some(HttpDataPlane::Tokio);
        assert!(DataPlanePolicy::resolve(tokio, nothing, available).is_ok());
    }

    #[test]
    fn io_uring_listeners_run_the_engine_on_io_uring() {
        let plane =
            policy(Some(HttpDataPlane::IoUring), MET).listener(Protocol::Http, &socket(), false);
        assert_eq!(engine(plane), Some(DataPlane::IoUring));
    }

    #[test]
    fn io_uring_is_never_the_default() {
        for supported in [true, false] {
            let conditions = EngineConditions { supported, ..MET };
            let plane = policy(None, conditions).listener(Protocol::Http, &socket(), false);
            assert_ne!(engine(plane), Some(DataPlane::IoUring));
        }
    }

    #[test]
    fn io_uring_listeners_that_the_ring_cannot_serve_stay_on_tokio() {
        let policy = policy(Some(HttpDataPlane::IoUring), MET);
        assert_eq!(
            engine(policy.listener(Protocol::Http, &socket(), true)),
            None
        );
        assert_eq!(
            engine(policy.listener(Protocol::Http3, &socket(), false)),
            None
        );
    }

    #[test]
    fn choosing_io_uring_names_the_capability_the_kernel_lacks() {
        let missing = || Some("the kernel does not support the io_uring Accept operation".into());
        let refused = DataPlanePolicy::resolve(Some(HttpDataPlane::IoUring), MET, missing);
        let Err(ServicePlanError::IoUringUnsupported { missing }) = refused else {
            panic!("io_uring must be refused");
        };
        assert!(missing.contains("Accept operation"));
    }

    #[test]
    fn choosing_io_uring_is_refused_without_the_fast_lane() {
        let conditions = EngineConditions {
            fast_lane: false,
            ..MET
        };
        let refused = DataPlanePolicy::resolve(Some(HttpDataPlane::IoUring), conditions, available);
        assert!(matches!(
            refused,
            Err(ServicePlanError::IoUringNeedsFastLane)
        ));
    }

    #[test]
    fn choosing_io_uring_is_refused_with_per_worker_scheduling() {
        let probe = || panic!("the kernel is not probed for a configuration that is refused");
        for plane in [None, Some("io_uring")] {
            let config = scheduled(plane);
            let refused = DataPlanePolicy::resolve(
                Some(HttpDataPlane::IoUring),
                EngineConditions::of(&config),
                probe,
            );
            assert!(matches!(
                refused,
                Err(ServicePlanError::IoUringNeedsSharedScheduling)
            ));
        }
    }

    #[test]
    fn nothing_but_choosing_io_uring_probes_the_kernel() {
        for setting in [None, Some(HttpDataPlane::Epoll), Some(HttpDataPlane::Tokio)] {
            let probe = || panic!("the kernel must not be probed for {setting:?}");
            assert!(DataPlanePolicy::resolve(setting, MET, probe).is_ok());
        }
    }
}
