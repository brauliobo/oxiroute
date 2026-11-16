use std::sync::atomic::{AtomicUsize, Ordering};

use oxiroute_config::{HttpDataPlane, ListenerBind, Protocol};

use super::{
    DataPlane, DataPlanePolicy, EngineConditions, ListenerPlane, PlaneSelection, Skip, Skipped,
    chain::{Link, PLANES, Planes, Probe},
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
const KINDS: [Kind; 7] = [
    Kind::PlainHttp,
    Kind::Tls,
    Kind::Http3,
    Kind::Unix,
    Kind::Scheduled,
    Kind::FastLaneOff,
    Kind::NoRoutes,
];
const RING_REFUSED: &str = "the kernel disables io_uring (kernel.io_uring_disabled = 2)";

fn granted() -> Option<String> {
    None
}

#[allow(clippy::unnecessary_wraps)]
fn ring_refused() -> Option<String> {
    Some(RING_REFUSED.to_owned())
}

#[allow(clippy::unnecessary_wraps)]
fn epoll_missing() -> Option<String> {
    Some("epoll is not available on this platform".to_owned())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kernel {
    Both,
    EpollOnly,
    Neither,
}

impl Kernel {
    fn probes(self) -> (Probe, Probe) {
        match self {
            Self::Both => (granted, granted),
            Self::EpollOnly => (ring_refused, granted),
            Self::Neither => (ring_refused, epoll_missing),
        }
    }

    fn planes(self, order: &'static [DataPlane]) -> Planes {
        let (ring, epoll) = self.probes();
        Planes {
            catalog: [
                Link {
                    probe: ring,
                    ..Link::IO_URING
                },
                Link {
                    probe: epoll,
                    ..Link::EPOLL
                },
                Link::TOKIO,
            ],
            order,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    PlainHttp,
    Tls,
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
            Self::Tls => policy.listener(Protocol::Http, &socket(), true),
            Self::Http3 => policy.listener(Protocol::Http3, &socket(), true),
            Self::Unix => policy.listener(Protocol::Http, &unix(), false),
            _ => policy.listener(Protocol::Http, &socket(), false),
        }
    }

    fn routes(self) -> bool {
        self != Self::NoRoutes
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
    planes: Planes,
) -> Result<PlaneSelection, ServicePlanError> {
    let policy = DataPlanePolicy::resolve(setting, kind.conditions(), planes)?;
    Ok(kind.listener(&policy).select(kind.routes()))
}

fn outcome(
    order: &'static [DataPlane],
    setting: Option<HttpDataPlane>,
    kernel: Kernel,
    kind: Kind,
) -> Outcome {
    match plan(setting, kind, kernel.planes(order)) {
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
    let served = |plane| {
        Outcome::Plane(if kind.engine_listener() {
            plane
        } else {
            DataPlane::Tokio
        })
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
        plan(None, Kind::PlainHttp, kernel.planes(SHIPPED))
            .expect("the chain never refuses")
            .plane
    };
    assert_eq!(chosen(Kernel::Both), DataPlane::IoUring);
    assert_eq!(chosen(Kernel::EpollOnly), DataPlane::Epoll);
    assert_eq!(chosen(Kernel::Neither), DataPlane::Tokio);
}

#[test]
fn leaving_io_uring_out_of_the_order_keeps_it_available_when_chosen_explicitly() {
    let planes = Kernel::Both.planes(WITHOUT_RING);
    let chosen = |setting| plan(setting, Kind::PlainHttp, planes).expect("valid").plane;
    assert_eq!(chosen(None), DataPlane::Epoll);
    assert_eq!(chosen(Some(HttpDataPlane::IoUring)), DataPlane::IoUring);
}

#[test]
fn a_skipped_plane_is_reported_with_the_reason_and_the_chosen_plane() {
    let planes = Kernel::EpollOnly.planes(SHIPPED);
    let selection = plan(None, Kind::PlainHttp, planes).expect("valid");
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
    let selection = plan(None, Kind::Tls, Kernel::Both.planes(SHIPPED)).expect("valid");
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
                reason: Skip::Tls
            },
        ]
    );
}

#[test]
fn scheduling_the_ring_cannot_run_leaves_the_listener_on_epoll() {
    let selection = plan(None, Kind::Scheduled, Kernel::Both.planes(SHIPPED)).expect("valid");
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
        Kernel::EpollOnly.planes(SHIPPED),
    );
    let message = refused.expect_err("io_uring is missing").to_string();
    assert!(message.contains("kernel.io_uring_disabled"), "{message}");
    assert!(message.contains("\"io_uring\""), "{message}");
}

static RING_PROBES: AtomicUsize = AtomicUsize::new(0);
static EPOLL_PROBES: AtomicUsize = AtomicUsize::new(0);

fn counted_ring() -> Option<String> {
    RING_PROBES.fetch_add(1, Ordering::Relaxed);
    None
}

fn counted_epoll() -> Option<String> {
    EPOLL_PROBES.fetch_add(1, Ordering::Relaxed);
    None
}

#[test]
fn a_generation_probes_only_the_planes_its_setting_can_choose_and_once() {
    let planes = Planes {
        catalog: [
            Link {
                probe: counted_ring,
                ..Link::IO_URING
            },
            Link {
                probe: counted_epoll,
                ..Link::EPOLL
            },
            Link::TOKIO,
        ],
        order: SHIPPED,
    };
    let probes = || {
        (
            RING_PROBES.load(Ordering::Relaxed),
            EPOLL_PROBES.load(Ordering::Relaxed),
        )
    };
    let conditions = EngineConditions {
        fast_lane: true,
        scheduling: false,
    };
    DataPlanePolicy::resolve(Some(HttpDataPlane::Tokio), conditions, planes).expect("valid");
    assert_eq!(probes(), (0, 0));
    DataPlanePolicy::resolve(Some(HttpDataPlane::Epoll), conditions, planes).expect("valid");
    assert_eq!(probes(), (0, 1));
    let policy = DataPlanePolicy::resolve(None, conditions, planes).expect("valid");
    assert_eq!(probes(), (1, 2));
    for _ in 0..3 {
        Kind::PlainHttp.listener(&policy).select(true);
    }
    assert_eq!(probes(), (1, 2));
}

#[test]
fn the_real_probes_agree_with_what_the_default_chain_selects() {
    let ring = oxiroute_epoll_h1::io_uring_support().is_ok();
    let epoll = oxiroute_epoll_h1::supported();
    let selected = plan(None, Kind::PlainHttp, PLANES).expect("valid").plane;
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
