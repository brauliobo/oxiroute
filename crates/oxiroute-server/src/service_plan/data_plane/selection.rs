use std::fmt;

use log::info;

use super::{DataPlane, Skip};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Skipped {
    pub plane: DataPlane,
    pub reason: Skip,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaneSelection {
    pub plane: DataPlane,
    pub skipped: Vec<Skipped>,
}

impl PlaneSelection {
    #[must_use]
    pub const fn tokio() -> Self {
        Self {
            plane: DataPlane::Tokio,
            skipped: Vec::new(),
        }
    }

    pub fn announce(&self, listener: &str) {
        info!("listener `{listener}` data plane: {self}");
    }
}

impl fmt::Display for Skipped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} skipped: {}",
            self.plane.as_str(),
            self.reason
        )
    }
}

impl fmt::Display for PlaneSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.plane.as_str())?;
        let mut skipped = self.skipped.iter();
        if let Some(first) = skipped.next() {
            write!(formatter, " ({first}")?;
            skipped.try_for_each(|skip| write!(formatter, "; {skip}"))?;
            write!(formatter, ")")?;
        }
        Ok(())
    }
}
