use oxiroute_config::{ConfigDraft, HttpDataPlane, ListenerBind, Protocol};

use super::{HttpServicePlan, ServicePlanError};

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

    pub(crate) fn plan(listener: ListenerPlane, service: &HttpServicePlan) -> Self {
        match listener.engine {
            Some(engine) if service.serves_on_engine() => engine,
            _ => Self::Tokio,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ListenerPlane {
    engine: Option<DataPlane>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DataPlanePolicy {
    setting: Option<HttpDataPlane>,
    fast_lane: bool,
    supported: bool,
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
            config.fast_lane_enabled(),
            oxiroute_epoll_h1::supported(),
            io_uring_unavailable,
        )
    }

    pub(crate) fn resolve(
        setting: Option<HttpDataPlane>,
        fast_lane: bool,
        supported: bool,
        io_uring_unavailable: impl FnOnce() -> Option<String>,
    ) -> Result<Self, ServicePlanError> {
        match setting {
            Some(HttpDataPlane::Epoll) if !supported => Err(ServicePlanError::EpollUnsupported),
            Some(HttpDataPlane::Epoll) if !fast_lane => Err(ServicePlanError::EpollNeedsFastLane),
            Some(HttpDataPlane::IoUring) if !fast_lane => {
                Err(ServicePlanError::IoUringNeedsFastLane)
            }
            Some(HttpDataPlane::IoUring) => match io_uring_unavailable() {
                Some(missing) => Err(ServicePlanError::IoUringUnsupported { missing }),
                None => Ok(Self::new(setting, fast_lane, supported)),
            },
            _ => Ok(Self::new(setting, fast_lane, supported)),
        }
    }

    const fn new(setting: Option<HttpDataPlane>, fast_lane: bool, supported: bool) -> Self {
        Self {
            setting,
            fast_lane,
            supported,
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
            && self.fast_lane
            && self.supported
            && protocol == Protocol::Http
            && !tls
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

    fn available() -> Option<String> {
        None
    }

    fn policy(setting: Option<HttpDataPlane>, fast_lane: bool, supported: bool) -> DataPlanePolicy {
        DataPlanePolicy::resolve(setting, fast_lane, supported, available).expect("a valid policy")
    }

    fn engine(plane: ListenerPlane) -> Option<DataPlane> {
        plane.engine
    }

    #[test]
    fn plain_http_listeners_default_to_epoll_where_it_is_supported() {
        let plane = policy(None, true, true).listener(Protocol::Http, &socket(), false);
        assert_eq!(engine(plane), Some(DataPlane::Epoll));
    }

    #[test]
    fn nothing_selects_epoll_where_it_is_not_supported() {
        let plane = policy(None, true, false).listener(Protocol::Http, &socket(), false);
        assert_eq!(engine(plane), None);
    }

    #[test]
    fn tokio_forces_the_tokio_path_everywhere() {
        let plane = policy(Some(HttpDataPlane::Tokio), true, true).listener(
            Protocol::Http,
            &socket(),
            false,
        );
        assert_eq!(engine(plane), None);
    }

    #[test]
    fn tls_http3_and_unix_listeners_stay_on_the_tokio_path() {
        let policy = policy(Some(HttpDataPlane::Epoll), true, true);
        assert_eq!(
            engine(policy.listener(Protocol::Http, &socket(), true)),
            None
        );
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
        let plane = policy(None, false, true).listener(Protocol::Http, &socket(), false);
        assert_eq!(engine(plane), None);
    }

    #[test]
    fn choosing_epoll_explicitly_is_refused_where_it_cannot_run() {
        let unsupported =
            DataPlanePolicy::resolve(Some(HttpDataPlane::Epoll), true, false, available);
        assert!(matches!(
            unsupported,
            Err(ServicePlanError::EpollUnsupported)
        ));
        let no_lane = DataPlanePolicy::resolve(Some(HttpDataPlane::Epoll), false, true, available);
        assert!(matches!(no_lane, Err(ServicePlanError::EpollNeedsFastLane)));
    }

    #[test]
    fn choosing_tokio_is_accepted_everywhere() {
        assert!(
            DataPlanePolicy::resolve(Some(HttpDataPlane::Tokio), false, false, available).is_ok()
        );
    }

    #[test]
    fn io_uring_listeners_run_the_engine_on_io_uring() {
        let plane = policy(Some(HttpDataPlane::IoUring), true, true).listener(
            Protocol::Http,
            &socket(),
            false,
        );
        assert_eq!(engine(plane), Some(DataPlane::IoUring));
    }

    #[test]
    fn io_uring_is_never_the_default() {
        for supported in [true, false] {
            let plane = policy(None, true, supported).listener(Protocol::Http, &socket(), false);
            assert_ne!(engine(plane), Some(DataPlane::IoUring));
        }
    }

    #[test]
    fn io_uring_listeners_that_the_engine_cannot_serve_stay_on_tokio() {
        let policy = policy(Some(HttpDataPlane::IoUring), true, true);
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
        let refused = DataPlanePolicy::resolve(Some(HttpDataPlane::IoUring), true, true, missing);
        let Err(ServicePlanError::IoUringUnsupported { missing }) = refused else {
            panic!("io_uring must be refused");
        };
        assert!(missing.contains("Accept operation"));
    }

    #[test]
    fn choosing_io_uring_is_refused_without_the_fast_lane() {
        let refused =
            DataPlanePolicy::resolve(Some(HttpDataPlane::IoUring), false, true, available);
        assert!(matches!(
            refused,
            Err(ServicePlanError::IoUringNeedsFastLane)
        ));
    }

    #[test]
    fn nothing_but_choosing_io_uring_probes_the_kernel() {
        for setting in [None, Some(HttpDataPlane::Epoll), Some(HttpDataPlane::Tokio)] {
            let probe = || panic!("the kernel must not be probed for {setting:?}");
            assert!(DataPlanePolicy::resolve(setting, true, true, probe).is_ok());
        }
    }
}
