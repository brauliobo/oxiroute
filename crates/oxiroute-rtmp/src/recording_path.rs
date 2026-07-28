use std::fmt::Write as _;

use chrono::{Datelike as _, TimeZone as _, Timelike as _, Utc};
use chrono_tz::Tz;

/// Maximum accepted byte length of a recording suffix template.
pub const MAX_RECORDING_SUFFIX_TEMPLATE_BYTES: usize = 128;
/// Maximum rendered byte length of one relative recording filename.
pub const MAX_RECORDING_FILENAME_BYTES: usize = 255;

pub(crate) fn sequenced_recording_filename(name: &str, sequence: u64) -> Option<String> {
    if sequence == 0 {
        return Some(name.to_owned());
    }
    insert_before_extension(name, &format!("-{sequence:06}"))
}

pub(crate) fn collision_recording_filename(name: &str, attempt: usize) -> Option<String> {
    insert_before_extension(name, &format!("-{attempt}"))
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum RecordingPathError {
    #[error("recording stream name must not be empty, `.` or `..`")]
    InvalidStreamName,
    #[error("recording stream name contains NUL")]
    NulInStreamName,
    #[error(
        "recording suffix template is {length} bytes; maximum is {MAX_RECORDING_SUFFIX_TEMPLATE_BYTES} bytes"
    )]
    SuffixTemplateTooLong { length: usize },
    #[error("recording suffix template contains NUL")]
    NulInSuffixTemplate,
    #[error("recording suffix template contains a path separator at byte {index}")]
    SuffixTemplateContainsSeparator { index: usize },
    #[error("invalid recording suffix format item at byte {index}")]
    InvalidSuffixFormat { index: usize },
    #[error("invalid recording UTC date-time {field} value {value}")]
    InvalidDateTime { field: &'static str, value: u16 },
    #[error(
        "rendered recording filename is {length} bytes; maximum is {MAX_RECORDING_FILENAME_BYTES} bytes"
    )]
    FilenameTooLong { length: usize },
}

/// Dependency-free UTC calendar fields injected by the recording caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordingDateTime {
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
}

impl RecordingDateTime {
    /// Converts a nonnegative Unix timestamp to UTC Gregorian calendar fields.
    ///
    /// # Errors
    ///
    /// Returns an error when the timestamp is later than `9999-12-31T23:59:59Z`.
    pub fn from_unix_seconds(seconds: u64) -> Result<Self, RecordingPathError> {
        const SECONDS_PER_DAY: u64 = 86_400;
        const MAX_UNIX_SECONDS: u64 = 253_402_300_799;

        if seconds > MAX_UNIX_SECONDS {
            return Err(invalid_date_time("year", 10_000));
        }

        let days = i64::try_from(seconds / SECONDS_PER_DAY)
            .map_err(|_| invalid_date_time("year", 10_000))?;
        let seconds_in_day = seconds % SECONDS_PER_DAY;
        let (year, month, day) = civil_from_unix_days(days);
        Self::new(
            u16::try_from(year).map_err(|_| invalid_date_time("year", 10_000))?,
            month,
            day,
            (seconds_in_day / 3_600) as u8,
            (seconds_in_day % 3_600 / 60) as u8,
            (seconds_in_day % 60) as u8,
        )
    }

    /// Validates bounded UTC-style calendar fields used by suffix formatting.
    ///
    /// # Errors
    ///
    /// Returns an error when a field cannot be represented by its fixed-width format token or the
    /// day does not exist in the supplied Gregorian month.
    pub fn new(
        year: u16,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) -> Result<Self, RecordingPathError> {
        if year > 9_999 {
            return Err(invalid_date_time("year", year));
        }
        if !(1..=12).contains(&month) {
            return Err(invalid_date_time("month", u16::from(month)));
        }
        if day == 0 || day > days_in_month(year, month) {
            return Err(invalid_date_time("day", u16::from(day)));
        }
        if hour > 23 {
            return Err(invalid_date_time("hour", u16::from(hour)));
        }
        if minute > 59 {
            return Err(invalid_date_time("minute", u16::from(minute)));
        }
        if second > 59 {
            return Err(invalid_date_time("second", u16::from(second)));
        }

        Ok(Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
        })
    }
}

fn civil_from_unix_days(days: i64) -> (i64, u8, u8) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (
        year,
        u8::try_from(month).expect("Gregorian month fits in u8"),
        u8::try_from(day).expect("Gregorian day fits in u8"),
    )
}

/// Validated policy for deriving one relative recording filename from RTMP stream bytes.
///
/// Native `record_unique` behavior appends open Unix seconds, which can collide when a stream is
/// opened more than once in one second. A future storage layer must still use an atomic,
/// collision-safe creation policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingPathPolicy {
    suffix_template: String,
    native_unique_seconds: bool,
    timezone: RecordingTimezone,
    time_basis: RecordingTimeBasis,
    segment_naming: RecordingSegmentNaming,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecordingTimezone {
    #[default]
    Utc,
    Iana(Tz),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecordingTimeBasis {
    #[default]
    SegmentStart,
    SegmentEnd,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecordingSegmentNaming {
    #[default]
    SafeUnique,
    NginxCompatible,
}

impl RecordingPathPolicy {
    /// Validates and retains a bounded recording suffix template.
    ///
    /// Supported format items are the intentionally bounded UTC subset `%Y`, `%m`, `%d`, `%H`,
    /// `%M`, `%S`, and `%%`. This is not general or local-time `strftime` compatibility; a native
    /// configuration importer must reject unsupported semantics rather than silently accept them.
    /// Enabling `native_unique_seconds` reproduces the native seconds suffix, not collision-free
    /// storage naming.
    ///
    /// # Errors
    ///
    /// Returns an error if the template exceeds its byte limit, contains NUL or a path separator,
    /// or uses any unsupported percent format item.
    pub fn new(
        suffix_template: &str,
        native_unique_seconds: bool,
    ) -> Result<Self, RecordingPathError> {
        validate_suffix_template(suffix_template)?;
        Ok(Self {
            suffix_template: suffix_template.to_owned(),
            native_unique_seconds,
            timezone: RecordingTimezone::Utc,
            time_basis: RecordingTimeBasis::SegmentStart,
            segment_naming: RecordingSegmentNaming::SafeUnique,
        })
    }

    #[must_use]
    pub fn with_segment_policy(
        mut self,
        timezone: RecordingTimezone,
        time_basis: RecordingTimeBasis,
        segment_naming: RecordingSegmentNaming,
    ) -> Self {
        self.timezone = timezone;
        self.time_basis = time_basis;
        self.segment_naming = segment_naming;
        self
    }

    /// Renders one relative filename without reading a clock or accessing the filesystem.
    ///
    /// The complete stream name is encoded as one component using uppercase percent escapes for
    /// bytes outside the RFC 3986 unreserved set; a leading dot is also escaped to prevent a hidden
    /// Unix filename. Protocol query separation belongs to [`crate::RtmpStreamPath`] and is not
    /// repeated here.
    ///
    /// # Errors
    ///
    /// Returns an error if the stream name is invalid or contains NUL, or if the rendered filename
    /// exceeds 255 bytes.
    pub fn relative_filename(
        &self,
        stream_name: &[u8],
        opened_at_unix_seconds: u64,
        opened_at_utc: RecordingDateTime,
    ) -> Result<String, RecordingPathError> {
        if stream_name.contains(&0) {
            return Err(RecordingPathError::NulInStreamName);
        }
        if stream_name.is_empty() || stream_name == b"." || stream_name == b".." {
            return Err(RecordingPathError::InvalidStreamName);
        }

        let suffix = self.render_suffix(opened_at_utc);
        let native_seconds_suffix = self
            .native_unique_seconds
            .then(|| format!("-{opened_at_unix_seconds}"))
            .unwrap_or_default();
        let escaped_length =
            stream_name
                .iter()
                .enumerate()
                .fold(0_usize, |length, (index, byte)| {
                    length.saturating_add(if can_emit_unescaped(index, *byte) {
                        1
                    } else {
                        3
                    })
                });
        let length = escaped_length
            .saturating_add(native_seconds_suffix.len())
            .saturating_add(suffix.len());
        if length > MAX_RECORDING_FILENAME_BYTES {
            return Err(RecordingPathError::FilenameTooLong { length });
        }

        let mut filename = String::with_capacity(length);
        percent_escape_into(stream_name, &mut filename);
        filename.push_str(&native_seconds_suffix);
        filename.push_str(&suffix);
        Ok(filename)
    }

    /// Renders a filename from an explicit Unix instant using this policy's fixed timezone.
    ///
    /// # Errors
    ///
    /// Returns the same bounded path and calendar errors as [`Self::relative_filename`].
    pub fn relative_filename_at(
        &self,
        stream_name: &[u8],
        at_unix_seconds: u64,
    ) -> Result<String, RecordingPathError> {
        let date_time = match self.timezone {
            RecordingTimezone::Utc => RecordingDateTime::from_unix_seconds(at_unix_seconds)?,
            RecordingTimezone::Iana(timezone) => zoned_date_time(at_unix_seconds, timezone)?,
        };
        self.relative_filename(stream_name, at_unix_seconds, date_time)
    }

    pub(crate) fn segment_filename(
        &self,
        stream_name: &[u8],
        at_unix_seconds: u64,
        _fallback_utc: RecordingDateTime,
        sequence: u64,
    ) -> Result<String, RecordingPathError> {
        let name = self.relative_filename_at(stream_name, at_unix_seconds)?;
        if self.segment_naming == RecordingSegmentNaming::SafeUnique {
            sequenced_recording_filename(&name, sequence)
                .ok_or(RecordingPathError::FilenameTooLong { length: usize::MAX })
        } else {
            Ok(name)
        }
    }

    pub(crate) const fn time_basis(&self) -> RecordingTimeBasis {
        self.time_basis
    }

    fn render_suffix(&self, opened_at: RecordingDateTime) -> String {
        let bytes = self.suffix_template.as_bytes();
        let mut rendered = String::with_capacity(bytes.len());
        let mut literal_start = 0;
        let mut index = 0;

        while index < bytes.len() {
            if bytes[index] != b'%' {
                index += 1;
                continue;
            }

            rendered.push_str(&self.suffix_template[literal_start..index]);
            match bytes[index + 1] {
                b'Y' => write!(rendered, "{:04}", opened_at.year),
                b'm' => write!(rendered, "{:02}", opened_at.month),
                b'd' => write!(rendered, "{:02}", opened_at.day),
                b'H' => write!(rendered, "{:02}", opened_at.hour),
                b'M' => write!(rendered, "{:02}", opened_at.minute),
                b'S' => write!(rendered, "{:02}", opened_at.second),
                b'%' => {
                    rendered.push('%');
                    Ok(())
                }
                _ => unreachable!("validated suffix templates contain only supported tokens"),
            }
            .expect("writing to a String cannot fail");
            index += 2;
            literal_start = index;
        }
        rendered.push_str(&self.suffix_template[literal_start..]);
        rendered
    }
}

fn zoned_date_time(seconds: u64, timezone: Tz) -> Result<RecordingDateTime, RecordingPathError> {
    let seconds = i64::try_from(seconds).map_err(|_| invalid_date_time("year", 10_000))?;
    let utc = Utc
        .timestamp_opt(seconds, 0)
        .single()
        .ok_or_else(|| invalid_date_time("year", 10_000))?;
    let local = utc.with_timezone(&timezone);
    RecordingDateTime::new(
        u16::try_from(local.year()).map_err(|_| invalid_date_time("year", 10_000))?,
        u8::try_from(local.month()).expect("calendar month fits u8"),
        u8::try_from(local.day()).expect("calendar day fits u8"),
        u8::try_from(local.hour()).expect("calendar hour fits u8"),
        u8::try_from(local.minute()).expect("calendar minute fits u8"),
        u8::try_from(local.second()).expect("calendar second fits u8"),
    )
}

fn validate_suffix_template(suffix_template: &str) -> Result<(), RecordingPathError> {
    let bytes = suffix_template.as_bytes();
    if bytes.len() > MAX_RECORDING_SUFFIX_TEMPLATE_BYTES {
        return Err(RecordingPathError::SuffixTemplateTooLong {
            length: bytes.len(),
        });
    }
    if bytes.contains(&0) {
        return Err(RecordingPathError::NulInSuffixTemplate);
    }
    if let Some(index) = bytes.iter().position(|byte| matches!(*byte, b'/' | b'\\')) {
        return Err(RecordingPathError::SuffixTemplateContainsSeparator { index });
    }

    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if !matches!(
            bytes.get(index + 1),
            Some(b'Y' | b'm' | b'd' | b'H' | b'M' | b'S' | b'%')
        ) {
            return Err(RecordingPathError::InvalidSuffixFormat { index });
        }
        index += 2;
    }
    Ok(())
}

fn percent_escape_into(bytes: &[u8], output: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    for (index, byte) in bytes.iter().enumerate() {
        if can_emit_unescaped(index, *byte) {
            output.push(char::from(*byte));
        } else {
            output.push('%');
            output.push(char::from(HEX[usize::from(*byte >> 4)]));
            output.push(char::from(HEX[usize::from(*byte & 0x0f)]));
        }
    }
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

fn can_emit_unescaped(index: usize, byte: u8) -> bool {
    is_unreserved(byte) && (index != 0 || byte != b'.')
}

fn invalid_date_time(field: &'static str, value: u16) -> RecordingPathError {
    RecordingPathError::InvalidDateTime { field, value }
}

fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        _ => unreachable!("month is validated before its day"),
    }
}

fn insert_before_extension(name: &str, insertion: &str) -> Option<String> {
    let extension_start = name.rfind('.').filter(|index| *index > 0);
    let (stem, extension) = extension_start.map_or((name, ""), |index| name.split_at(index));
    let maximum_stem =
        MAX_RECORDING_FILENAME_BYTES.checked_sub(insertion.len() + extension.len())?;
    if maximum_stem == 0 {
        return None;
    }
    let mut stem_end = stem.len().min(maximum_stem);
    while !stem.is_char_boundary(stem_end) {
        stem_end -= 1;
    }
    if stem_end == 0 {
        return None;
    }
    Some(format!("{}{insertion}{extension}", &stem[..stem_end]))
}
