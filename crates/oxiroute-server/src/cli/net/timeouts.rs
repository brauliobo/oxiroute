use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Timeouts {
    connect: Duration,
    answer: Duration,
    wait: Duration,
}

impl Timeouts {
    pub(crate) const DEFAULT: Self = Self {
        connect: Duration::from_secs(5),
        answer: Duration::from_secs(10),
        wait: Duration::from_secs(30),
    };

    pub(crate) const fn limited(limit: Duration) -> Self {
        Self {
            connect: limit,
            answer: limit,
            wait: limit,
        }
    }

    pub(crate) const fn connect(self) -> Duration {
        self.connect
    }

    pub(crate) const fn answer(self) -> Duration {
        self.answer
    }

    pub(crate) const fn wait(self) -> Duration {
        self.wait
    }
}

pub(crate) fn span(duration: Duration) -> String {
    let milliseconds = duration.as_millis();
    if milliseconds.is_multiple_of(1_000) {
        format!("{}s", milliseconds / 1_000)
    } else {
        format!("{milliseconds}ms")
    }
}
