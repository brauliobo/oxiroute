mod chain;
mod selection;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use oxiroute_config::{ConfigDraft, HttpDataPlane, ListenerBind, Protocol};

use self::chain::{Candidate, ListenerShape, PLANES, Planes};
pub use self::{
    chain::Skip,
    selection::{PlaneSelection, Skipped},
};
use super::ServicePlanError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataPlane {
    Tokio,
    Epoll,
    IoUring,
}

impl DataPlane {
    pub const DEFAULT_ORDER: &'static [Self] = &[Self::Epoll, Self::Tokio];

    pub(crate) const fn is(self, setting: HttpDataPlane) -> bool {
        matches!(
            (self, setting),
            (Self::Tokio, HttpDataPlane::Tokio)
                | (Self::Epoll, HttpDataPlane::Epoll)
                | (Self::IoUring, HttpDataPlane::IoUring)
        )
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tokio => "tokio",
            Self::Epoll => "epoll",
            Self::IoUring => "io_uring",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct EngineConditions {
    pub(crate) fast_lane: bool,
    pub(crate) scheduling: bool,
}

impl EngineConditions {
    fn of(config: &ConfigDraft) -> Self {
        Self {
            fast_lane: config.fast_lane_enabled(),
            scheduling: config.aged_per_worker(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DataPlanePolicy {
    candidates: Arc<[Candidate]>,
}

impl DataPlanePolicy {
    pub(crate) fn of(config: &ConfigDraft) -> Result<Self, ServicePlanError> {
        Self::resolve(
            config.http_data_plane_choice(),
            EngineConditions::of(config),
            PLANES,
        )
    }

    pub(crate) fn resolve(
        setting: Option<HttpDataPlane>,
        conditions: EngineConditions,
        planes: Planes,
    ) -> Result<Self, ServicePlanError> {
        let candidates = planes.candidates(setting, conditions);
        refuse_explicit_plane(setting, &candidates)?;
        Ok(Self {
            candidates: candidates.into(),
        })
    }

    pub(crate) fn listener(
        &self,
        protocol: Protocol,
        bind: &ListenerBind,
        tls: bool,
    ) -> ListenerPlane {
        ListenerPlane {
            candidates: Arc::clone(&self.candidates),
            shape: ListenerShape::of(protocol, bind, tls),
        }
    }
}

fn refuse_explicit_plane(
    setting: Option<HttpDataPlane>,
    candidates: &[Candidate],
) -> Result<(), ServicePlanError> {
    let Some(plane) = setting else {
        return Ok(());
    };
    match candidates.first().and_then(Candidate::unavailable) {
        Some(skip) => Err(ServicePlanError::DataPlaneUnavailable {
            plane: plane.as_str(),
            skip: skip.clone(),
        }),
        None => Ok(()),
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ListenerPlane {
    candidates: Arc<[Candidate]>,
    shape: ListenerShape,
}

impl ListenerPlane {
    pub(crate) fn select(&self, routes: bool, terminable: bool) -> PlaneSelection {
        let mut skipped = Vec::new();
        for candidate in self.candidates.iter() {
            match candidate.admits(self.shape, routes, terminable) {
                Ok(()) => {
                    return PlaneSelection {
                        plane: candidate.plane(),
                        skipped,
                    };
                }
                Err(reason) => skipped.push(Skipped {
                    plane: candidate.plane(),
                    reason,
                }),
            }
        }
        unreachable!("every chain ends in tokio, which serves every listener")
    }
}
