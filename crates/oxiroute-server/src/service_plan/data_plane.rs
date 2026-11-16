use oxiroute_config::{ConfigDraft, HttpDataPlane, ListenerBind, Protocol};

use super::{HttpServicePlan, ServicePlanError};

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

    pub(crate) fn plan(listener: ListenerPlane, service: &HttpServicePlan) -> Self {
        if listener.epoll && service.serves_on_engine() {
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
pub(crate) struct EngineConditions {
    pub(crate) supported: bool,
    pub(crate) fast_lane: bool,
}

impl EngineConditions {
    fn of(config: &ConfigDraft) -> Self {
        Self {
            supported: oxiroute_epoll_h1::supported(),
            fast_lane: config.fast_lane_enabled(),
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

impl DataPlanePolicy {
    pub(crate) fn of(config: &ConfigDraft) -> Result<Self, ServicePlanError> {
        Self::resolve(
            config.http_data_plane_choice(),
            EngineConditions::of(config),
        )
    }

    pub(crate) fn resolve(
        setting: Option<HttpDataPlane>,
        conditions: EngineConditions,
    ) -> Result<Self, ServicePlanError> {
        match setting {
            Some(HttpDataPlane::Epoll) if !conditions.supported => {
                Err(ServicePlanError::EpollUnsupported)
            }
            Some(HttpDataPlane::Epoll) if !conditions.fast_lane => {
                Err(ServicePlanError::EpollNeedsFastLane)
            }
            _ => Ok(Self {
                setting,
                conditions,
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
                && self.conditions.met()
                && protocol == Protocol::Http
                && !tls
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

    const MET: EngineConditions = EngineConditions {
        supported: true,
        fast_lane: true,
    };

    fn policy(setting: Option<HttpDataPlane>, conditions: EngineConditions) -> DataPlanePolicy {
        DataPlanePolicy::resolve(setting, conditions).expect("a valid policy")
    }

    #[test]
    fn plain_http_listeners_default_to_epoll_where_it_is_supported() {
        let plane = policy(None, MET).listener(Protocol::Http, &socket(), false);
        assert!(plane.epoll);
    }

    #[test]
    fn nothing_selects_epoll_where_it_is_not_supported() {
        let conditions = EngineConditions {
            supported: false,
            ..MET
        };
        let plane = policy(None, conditions).listener(Protocol::Http, &socket(), false);
        assert!(!plane.epoll);
    }

    #[test]
    fn tokio_forces_the_tokio_path_everywhere() {
        let plane =
            policy(Some(HttpDataPlane::Tokio), MET).listener(Protocol::Http, &socket(), false);
        assert!(!plane.epoll);
    }

    #[test]
    fn tls_http3_and_unix_listeners_stay_on_the_tokio_path() {
        let policy = policy(Some(HttpDataPlane::Epoll), MET);
        assert!(!policy.listener(Protocol::Http, &socket(), true).epoll);
        assert!(!policy.listener(Protocol::Http3, &socket(), false).epoll);
        let unix = ListenerBind::Unix {
            path: "/run/example.sock".into(),
            mode: None,
        };
        assert!(!policy.listener(Protocol::Http, &unix, false).epoll);
    }

    #[test]
    fn the_fast_lane_off_keeps_the_default_on_the_tokio_path() {
        let conditions = EngineConditions {
            fast_lane: false,
            ..MET
        };
        let plane = policy(None, conditions).listener(Protocol::Http, &socket(), false);
        assert!(!plane.epoll);
    }

    #[test]
    fn choosing_epoll_explicitly_is_refused_where_it_cannot_run() {
        let epoll = Some(HttpDataPlane::Epoll);
        let refused = |conditions| DataPlanePolicy::resolve(epoll, conditions);
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
            let policy = DataPlanePolicy::resolve(config.http_data_plane_choice(), MET)
                .expect("scheduling is not a condition of the engine");
            assert!(policy.listener(Protocol::Http, &socket(), false).epoll);
        }
    }

    #[test]
    fn choosing_tokio_is_accepted_everywhere() {
        let nothing = EngineConditions {
            supported: false,
            fast_lane: false,
        };
        assert!(DataPlanePolicy::resolve(Some(HttpDataPlane::Tokio), nothing).is_ok());
    }
}
