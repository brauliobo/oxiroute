use oxiroute_config::{HttpDataPlane, ListenerBind, Protocol};

use super::{
    DataPlane, DataPlanePolicy, EngineConditions, ListenerPlane, MissingCapability,
    PlaneCapabilities, PlaneSelection, Skip, Skipped,
    chain::{Link, PLANES, Planes},
};
use crate::ServicePlanError;

const SHIPPED: &[DataPlane] = &[DataPlane::IoUring, DataPlane::Epoll, DataPlane::Tokio];
const WITHOUT_RING: &[DataPlane] = &[DataPlane::Epoll, DataPlane::Tokio];
const ORDERS: [&[DataPlane]; 2] = [SHIPPED, WITHOUT_RING];
const SETTINGS: [Option<HttpDataPlane>; 4] = [
    None,
    Some(HttpDataPlane::IoUring),
    Some(HttpDataPlane::Epoll),
    Some(HttpDataPlane::Tokio),
];
const KERNELS: [Kernel; 3] = [Kernel::Both, Kernel::EpollOnly, Kernel::Neither];
const KINDS: [Kind; 8] = [
    Kind::PlainHttp,
    Kind::Tls,
    Kind::TlsOpenssl,
    Kind::Http3,
    Kind::Unix,
    Kind::Scheduled,
    Kind::FastLaneOff,
    Kind::NoRoutes,
];
const RING_REFUSED: &str = "the kernel disables io_uring (kernel.io_uring_disabled = 2)";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kernel {
    Both,
    EpollOnly,
    Neither,
}

impl Kernel {
    fn capabilities(self) -> PlaneCapabilities {
        let ring = MissingCapability(RING_REFUSED.to_owned());
        let epoll = MissingCapability("epoll is not available on this platform".to_owned());
        match self {
            Self::Both => PlaneCapabilities::known(Ok(()), Ok(())),
            Self::EpollOnly => PlaneCapabilities::known(Ok(()), Err(ring)),
            Self::Neither => PlaneCapabilities::known(Err(epoll), Err(ring)),
        }
    }

    fn planes(order: &'static [DataPlane]) -> Planes {
        Planes {
            catalog: [Link::IO_URING, Link::EPOLL, Link::TOKIO],
            order,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    PlainHttp,
    Tls,
    TlsOpenssl,
    Http3,
    Unix,
    Scheduled,
    FastLaneOff,
    NoRoutes,
}

fn socket() -> ListenerBind {
    ListenerBind::Socket {
        address: std::net::SocketAddr::from(([127, 0, 0, 1], 8080)),
        ipv6_only: false,
    }
}

fn unix() -> ListenerBind {
    ListenerBind::Unix {
        path: "/run/example.sock".into(),
        mode: None,
    }
}

impl Kind {
    fn conditions(self) -> EngineConditions {
        EngineConditions {
            fast_lane: self != Self::FastLaneOff,
            scheduling: self == Self::Scheduled,
        }
    }

    fn listener(self, policy: &DataPlanePolicy) -> ListenerPlane {
        match self {
            Self::Tls | Self::TlsOpenssl => policy.listener(Protocol::Http, &socket(), true),
            Self::Http3 => policy.listener(Protocol::Http3, &socket(), true),
            Self::Unix => policy.listener(Protocol::Http, &unix(), false),
            _ => policy.listener(Protocol::Http, &socket(), false),
        }
    }

    fn routes(self) -> bool {
        self != Self::NoRoutes
    }

    fn terminable(self) -> bool {
        self != Self::TlsOpenssl
    }

    fn engine_listener(self) -> bool {
        matches!(self, Self::PlainHttp | Self::Scheduled)
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Refusal {
    Missing,
    FastLane,
    Scheduling,
}

#[derive(Debug, Eq, PartialEq)]
enum Outcome {
    Plane(DataPlane),
    Refused(Refusal),
}

fn plan(
    setting: Option<HttpDataPlane>,
    kind: Kind,
    kernel: Kernel,
    order: &'static [DataPlane],
) -> Result<PlaneSelection, ServicePlanError> {
    let policy = DataPlanePolicy::evaluate(
        setting,
        &kernel.capabilities(),
        kind.conditions(),
        Kernel::planes(order),
    )?;
    Ok(kind
        .listener(&policy)
        .select(kind.routes(), kind.terminable()))
}

fn outcome(
    order: &'static [DataPlane],
    setting: Option<HttpDataPlane>,
    kernel: Kernel,
    kind: Kind,
) -> Outcome {
    match plan(setting, kind, kernel, order) {
        Ok(selection) => Outcome::Plane(selection.plane),
        Err(ServicePlanError::DataPlaneUnavailable { skip, .. }) => Outcome::Refused(match skip {
            Skip::Unavailable(_) => Refusal::Missing,
            Skip::FastLaneOff => Refusal::FastLane,
            Skip::Scheduling => Refusal::Scheduling,
            other => panic!("a listener reason is no refusal: {other}"),
        }),
        Err(other) => panic!("unexpected error: {other}"),
    }
}

fn expected(
    order: &[DataPlane],
    setting: Option<HttpDataPlane>,
    kernel: Kernel,
    kind: Kind,
) -> Outcome {
    let ring = kernel == Kernel::Both;
    let epoll = kernel != Kernel::Neither;
    let epoll_tls = kind == Kind::Tls && setting == Some(HttpDataPlane::Epoll);
    let served = |plane| {
        Outcome::Plane(
            if kind.engine_listener() || (epoll_tls && plane == DataPlane::Epoll) {
                plane
            } else {
                DataPlane::Tokio
            },
        )
    };
    match setting {
        Some(HttpDataPlane::Tokio) => Outcome::Plane(DataPlane::Tokio),
        Some(HttpDataPlane::IoUring) if !ring => Outcome::Refused(Refusal::Missing),
        Some(HttpDataPlane::Epoll) if !epoll => Outcome::Refused(Refusal::Missing),
        Some(_) if kind == Kind::FastLaneOff => Outcome::Refused(Refusal::FastLane),
        Some(HttpDataPlane::IoUring) if kind == Kind::Scheduled => {
            Outcome::Refused(Refusal::Scheduling)
        }
        Some(HttpDataPlane::IoUring) => served(DataPlane::IoUring),
        Some(HttpDataPlane::Epoll) => served(DataPlane::Epoll),
        None => {
            let usable = |plane: &&DataPlane| match plane {
                DataPlane::IoUring => ring && kind.engine_listener() && kind != Kind::Scheduled,
                DataPlane::Epoll => epoll && kind.engine_listener(),
                DataPlane::Tokio => true,
            };
            Outcome::Plane(*order.iter().find(usable).expect("tokio ends every order"))
        }
    }
}

#[test]
fn every_setting_kernel_and_listener_kind_gets_the_documented_plane_or_refusal() {
    for order in ORDERS {
        for setting in SETTINGS {
            for kernel in KERNELS {
                for kind in KINDS {
                    assert_eq!(
                        outcome(order, setting, kernel, kind),
                        expected(order, setting, kernel, kind),
                        "{order:?} {setting:?} {kernel:?} {kind:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_shipped_order_is_one_of_the_two_tested_orders() {
    assert!(ORDERS.contains(&DataPlane::DEFAULT_ORDER));
    assert_eq!(PLANES.order, DataPlane::DEFAULT_ORDER);
}

#[test]
fn an_omitted_setting_takes_io_uring_then_epoll_then_tokio_in_the_shipped_order() {
    let chosen = |kernel: Kernel| {
        plan(None, Kind::PlainHttp, kernel, SHIPPED)
            .expect("the chain never refuses")
            .plane
    };
    assert_eq!(chosen(Kernel::Both), DataPlane::IoUring);
    assert_eq!(chosen(Kernel::EpollOnly), DataPlane::Epoll);
    assert_eq!(chosen(Kernel::Neither), DataPlane::Tokio);
}

#[test]
fn leaving_io_uring_out_of_the_order_keeps_it_available_when_chosen_explicitly() {
    let chosen = |setting| {
        plan(setting, Kind::PlainHttp, Kernel::Both, WITHOUT_RING)
            .expect("valid")
            .plane
    };
    assert_eq!(chosen(None), DataPlane::Epoll);
    assert_eq!(chosen(Some(HttpDataPlane::IoUring)), DataPlane::IoUring);
}

#[test]
fn a_skipped_plane_is_reported_with_the_reason_and_the_chosen_plane() {
    let selection = plan(None, Kind::PlainHttp, Kernel::EpollOnly, SHIPPED).expect("valid");
    assert_eq!(selection.plane, DataPlane::Epoll);
    assert_eq!(
        selection.skipped,
        [Skipped {
            plane: DataPlane::IoUring,
            reason: Skip::Unavailable(RING_REFUSED.to_owned()),
        }]
    );
    assert_eq!(
        selection.to_string(),
        format!("epoll (io_uring skipped: {RING_REFUSED})")
    );
}

#[test]
fn every_better_plane_that_cannot_serve_a_listener_is_reported() {
    let selection = plan(None, Kind::Tls, Kernel::Both, SHIPPED).expect("valid");
    assert_eq!(selection.plane, DataPlane::Tokio);
    assert_eq!(
        selection.skipped,
        [
            Skipped {
                plane: DataPlane::IoUring,
                reason: Skip::Tls
            },
            Skipped {
                plane: DataPlane::Epoll,
                reason: Skip::TlsNotChosen
            },
        ]
    );
}

#[test]
fn scheduling_the_ring_cannot_run_leaves_the_listener_on_epoll() {
    let selection = plan(None, Kind::Scheduled, Kernel::Both, SHIPPED).expect("valid");
    assert_eq!(selection.plane, DataPlane::Epoll);
    assert_eq!(
        selection.skipped,
        [Skipped {
            plane: DataPlane::IoUring,
            reason: Skip::Scheduling
        }]
    );
}

#[test]
fn an_explicit_refusal_names_the_missing_capability() {
    let refused = plan(
        Some(HttpDataPlane::IoUring),
        Kind::PlainHttp,
        Kernel::EpollOnly,
        SHIPPED,
    );
    let message = refused.expect_err("io_uring is missing").to_string();
    assert!(message.contains("kernel.io_uring_disabled"), "{message}");
    assert!(message.contains("\"io_uring\""), "{message}");
}

#[test]
fn the_host_detects_only_the_planes_the_setting_can_reach() {
    let detected = |setting| {
        let capabilities = PlaneCapabilities::detect(setting);
        (
            capabilities.epoll.is_some(),
            capabilities.io_uring.is_some(),
        )
    };
    assert_eq!(detected(Some(HttpDataPlane::Tokio)), (false, false));
    assert_eq!(detected(Some(HttpDataPlane::Epoll)), (true, false));
    assert_eq!(detected(Some(HttpDataPlane::IoUring)), (false, true));
    assert_eq!(detected(None), (true, false));
}

#[test]
fn the_detected_capabilities_agree_with_what_the_default_chain_selects() {
    let ring = oxiroute_epoll_h1::io_uring_support().is_ok();
    let epoll = oxiroute_epoll_h1::supported();
    let capabilities = PlaneCapabilities::detect(None);
    let policy =
        DataPlanePolicy::evaluate(None, &capabilities, Kind::PlainHttp.conditions(), PLANES)
            .expect("valid");
    let selected = Kind::PlainHttp.listener(&policy).select(true, true).plane;
    let wanted = DataPlane::DEFAULT_ORDER
        .iter()
        .copied()
        .find(|plane| match plane {
            DataPlane::IoUring => ring,
            DataPlane::Epoll => epoll,
            DataPlane::Tokio => true,
        })
        .expect("tokio ends the order");
    assert_eq!(selected, wanted);
}
