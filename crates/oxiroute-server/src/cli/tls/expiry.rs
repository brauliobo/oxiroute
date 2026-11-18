use serde_json::Value;
use time::{Date, Month, PrimitiveDateTime, Time};

use crate::cli::units;

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const FIELDS: [&str; 3] = ["notAfterUnixSeconds", "expiresAt", "notAfter"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Expiry {
    unix_seconds: i64,
}

impl Expiry {
    pub(crate) const fn from_unix(unix_seconds: i64) -> Self {
        Self { unix_seconds }
    }

    /// The expiry an inventory entry reports: its unix seconds, or the OpenSSL date text `Jan  1 00:00:00 2030 GMT`.
    pub(crate) fn of_status(status: &Value) -> Option<Self> {
        FIELDS.iter().find_map(|field| {
            let value = status.get(*field)?;
            value
                .as_i64()
                .map(Self::from_unix)
                .or_else(|| Self::parse_openssl(value.as_str()?))
        })
    }

    pub(crate) fn parse_openssl(text: &str) -> Option<Self> {
        let mut words = text.split_whitespace();
        let month_name = words.next()?;
        let month = MONTHS.iter().position(|name| *name == month_name)?;
        let day = words.next()?.parse().ok()?;
        let mut clock = words.next()?.splitn(3, ':');
        let mut next = || clock.next()?.parse::<u8>().ok();
        let time = Time::from_hms(next()?, next()?, next()?).ok()?;
        let year = words.next()?.parse().ok()?;
        let zone = words.next()?;
        if zone != "GMT" || words.next().is_some() {
            return None;
        }
        let month = Month::try_from(u8::try_from(month + 1).ok()?).ok()?;
        let date = Date::from_calendar_date(year, month, day).ok()?;
        Some(Self::from_unix(
            PrimitiveDateTime::new(date, time)
                .assume_utc()
                .unix_timestamp(),
        ))
    }

    pub(crate) const fn unix_seconds(self) -> i64 {
        self.unix_seconds
    }

    pub(crate) fn utc(self) -> String {
        u64::try_from(self.unix_seconds)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1_000))
            .map_or_else(|| self.unix_seconds.to_string(), units::timestamp)
    }

    pub(crate) const fn remaining(self, now: i64) -> i64 {
        self.unix_seconds.saturating_sub(now)
    }

    /// `in 29d04h` before the expiry, `expired 2h05m ago` after it.
    pub(crate) fn relative(self, now: i64) -> String {
        let remaining = self.remaining(now);
        let span = units::duration(remaining.unsigned_abs().saturating_mul(1_000));
        if remaining >= 0 {
            format!("in {span}")
        } else {
            format!("expired {span} ago")
        }
    }
}
