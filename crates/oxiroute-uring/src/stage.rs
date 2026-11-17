use std::{error::Error, fmt, io};

pub(crate) const SETTING_UP_THE_RING: &str = "setting up the ring";

#[derive(Debug)]
pub(crate) struct Stage {
    step: &'static str,
    error: io::Error,
}

impl Stage {
    pub(crate) fn of(error: &io::Error) -> Option<&Self> {
        error.get_ref()?.downcast_ref()
    }

    pub(crate) fn is(&self, step: &str) -> bool {
        self.step == step
    }

    pub(crate) fn errno(&self) -> Option<i32> {
        self.error.raw_os_error()
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.step, self.error)
    }
}

impl Error for Stage {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

pub(crate) fn during(step: &'static str) -> impl FnOnce(io::Error) -> io::Error {
    move |error| io::Error::new(error.kind(), Stage { step, error })
}
