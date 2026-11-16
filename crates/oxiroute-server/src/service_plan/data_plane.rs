use oxiroute_config::{ConfigDraft, HttpDataPlane, ListenerBind, Protocol};

use super::{HttpServicePlan, ServicePlanError};
use crate::tls::TlsProfilePlan;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataPlane {
    Tokio,
    Epoll,
}

impl DataPlane {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tokio => "tokio",
            Self::Epoll => "epoll",
        }
    }

    pub(crate) fn plan(
        listener: ListenerPlane,
        service: &HttpServicePlan,
        tls: Option<&TlsProfilePlan>,
    ) -> Self {
        let terminable = tls.is_none_or(TlsProfilePlan::serves_on_epoll);
        if listener.epoll && terminable && service.serves_on_engine() {
            Self::Epoll
        } else {
            Self::Tokio
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ListenerPlane {
    epoll: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DataPlanePolicy {
    setting: Option<HttpDataPlane>,
    fast_lane: bool,
    supported: bool,
}

impl DataPlanePolicy {
    pub(crate) fn of(config: &ConfigDraft) -> Result<Self, ServicePlanError> {
        Self::resolve(
            config.http_data_plane_choice(),
            config.fast_lane_enabled(),
            oxiroute_epoll_h1::supported(),
        )
    }

    pub(crate) fn resolve(
        setting: Option<HttpDataPlane>,
        fast_lane: bool,
        supported: bool,
    ) -> Result<Self, ServicePlanError> {
        match setting {
            Some(HttpDataPlane::Epoll) if !supported => Err(ServicePlanError::EpollUnsupported),
            Some(HttpDataPlane::Epoll) if !fast_lane => Err(ServicePlanError::EpollNeedsFastLane),
            _ => Ok(Self {
                setting,
                fast_lane,
                supported,
            }),
        }
    }

    pub(crate) fn listener(
        self,
        protocol: Protocol,
        bind: &ListenerBind,
        tls: bool,
    ) -> ListenerPlane {
        ListenerPlane {
            epoll: self.setting != Some(HttpDataPlane::Tokio)
                && self.fast_lane
                && self.supported
                && protocol == Protocol::Http
                && (!tls || self.setting == Some(HttpDataPlane::Epoll))
                && matches!(bind, ListenerBind::Socket { .. }),
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

    fn policy(setting: Option<HttpDataPlane>, fast_lane: bool, supported: bool) -> DataPlanePolicy {
        DataPlanePolicy::resolve(setting, fast_lane, supported).expect("a valid policy")
    }

    #[test]
    fn plain_http_listeners_default_to_epoll_where_it_is_supported() {
        let plane = policy(None, true, true).listener(Protocol::Http, &socket(), false);
        assert!(plane.epoll);
    }

    #[test]
    fn nothing_selects_epoll_where_it_is_not_supported() {
        let plane = policy(None, true, false).listener(Protocol::Http, &socket(), false);
        assert!(!plane.epoll);
    }

    #[test]
    fn tokio_forces_the_tokio_path_everywhere() {
        let plane = policy(Some(HttpDataPlane::Tokio), true, true).listener(
            Protocol::Http,
            &socket(),
            false,
        );
        assert!(!plane.epoll);
    }

    #[test]
    fn tls_listeners_use_epoll_only_when_it_is_chosen_explicitly() {
        let explicit = policy(Some(HttpDataPlane::Epoll), true, true);
        assert!(explicit.listener(Protocol::Http, &socket(), true).epoll);
        let default = policy(None, true, true);
        assert!(!default.listener(Protocol::Http, &socket(), true).epoll);
    }

    #[test]
    fn http3_and_unix_listeners_stay_on_the_tokio_path() {
        let policy = policy(Some(HttpDataPlane::Epoll), true, true);
        assert!(!policy.listener(Protocol::Http3, &socket(), false).epoll);
        let unix = ListenerBind::Unix {
            path: "/run/example.sock".into(),
            mode: None,
        };
        assert!(!policy.listener(Protocol::Http, &unix, false).epoll);
    }

    #[test]
    fn the_fast_lane_off_keeps_the_default_on_the_tokio_path() {
        let plane = policy(None, false, true).listener(Protocol::Http, &socket(), false);
        assert!(!plane.epoll);
    }

    #[test]
    fn choosing_epoll_explicitly_is_refused_where_it_cannot_run() {
        let unsupported = DataPlanePolicy::resolve(Some(HttpDataPlane::Epoll), true, false);
        assert!(matches!(
            unsupported,
            Err(ServicePlanError::EpollUnsupported)
        ));
        let no_lane = DataPlanePolicy::resolve(Some(HttpDataPlane::Epoll), false, true);
        assert!(matches!(no_lane, Err(ServicePlanError::EpollNeedsFastLane)));
    }

    #[test]
    fn choosing_tokio_is_accepted_everywhere() {
        assert!(DataPlanePolicy::resolve(Some(HttpDataPlane::Tokio), false, false).is_ok());
    }
}
