use std::fmt::Write as _;

use chrono::{Datelike as _, TimeZone as _, Timelike as _, Utc};
use chrono_tz::Tz;
use sha2::{Digest as _, Sha256};

/// Maximum accepted byte length of a recording suffix template.
pub const MAX_RECORDING_SUFFIX_TEMPLATE_BYTES: usize = 128;
/// Maximum rendered byte length of one relative recording filename.
pub const MAX_RECORDING_FILENAME_BYTES: usize = 255;
const MAX_RECORDING_COLLISION_SUFFIX_BYTES: usize = 3;

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
        let native_seconds_suffix = if self.native_unique_seconds {
            format!("-{opened_at_unix_seconds}")
        } else {
            String::default()
        };
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
        let base = self.relative_filename_at(stream_name, at_unix_seconds)?;
        let sequence = if self.segment_naming == RecordingSegmentNaming::SafeUnique {
            sequence
        } else {
            0
        };
        let mut escaped_stream = String::new();
        percent_escape_into(stream_name, &mut escaped_stream);
        let identity_length = escaped_stream
            .len()
            .saturating_add(usize::from(self.native_unique_seconds))
            .saturating_add(
                self.native_unique_seconds
                    .then_some(at_unix_seconds)
                    .map_or(0, |seconds| seconds.to_string().len()),
            );
        let extension_start = base
            .rfind('.')
            .filter(|extension_start| *extension_start >= identity_length);
        bounded_segment_filename(&base, at_unix_seconds, sequence, extension_start)
            .ok_or(RecordingPathError::FilenameTooLong { length: usize::MAX })
    }

    pub(crate) const fn time_basis(&self) -> RecordingTimeBasis {
        self.time_basis
    }

    pub(crate) fn segment_identity_from_filename(
        &self,
        stream_name: &[u8],
        filename: &str,
    ) -> Option<(u64, u64, usize)> {
        if !self.native_unique_seconds || self.time_basis != RecordingTimeBasis::SegmentStart {
            return None;
        }
        if let Some(identity) = self.compact_segment_identity(stream_name, filename) {
            return Some(identity);
        }
        let mut prefix = String::new();
        percent_escape_into(stream_name, &mut prefix);
        prefix.push('-');
        if let Some(remainder) = filename.strip_prefix(&prefix) {
            let seconds_length = remainder.bytes().take_while(u8::is_ascii_digit).count();
            for length in 1..=seconds_length {
                let Ok(started_at) = remainder[..length].parse() else {
                    continue;
                };
                let Ok(base) = self.relative_filename_at(stream_name, started_at) else {
                    continue;
                };
                if let Some((sequence, collision)) = self.segment_variant_identity(&base, filename)
                {
                    return Some((started_at, sequence, collision));
                }
            }
        }
        None
    }

    fn compact_segment_identity(
        &self,
        stream_name: &[u8],
        filename: &str,
    ) -> Option<(u64, u64, usize)> {
        let collision_candidate = numeric_suffix(filename).and_then(|(candidate, collision)| {
            usize::try_from(collision)
                .ok()
                .map(|collision| (candidate, collision))
        });
        std::iter::once((filename.to_owned(), 0))
            .chain(collision_candidate)
            .find_map(|(candidate, collision)| {
                compact_identity_candidates(&candidate).find_map(|(started_at, sequence)| {
                    if self.segment_naming != RecordingSegmentNaming::SafeUnique && sequence != 0 {
                        return None;
                    }
                    let expected = self
                        .segment_filename(
                            stream_name,
                            started_at,
                            RecordingDateTime::from_unix_seconds(started_at).ok()?,
                            sequence,
                        )
                        .ok()?;
                    let expected = if collision == 0 {
                        expected
                    } else {
                        collision_recording_filename(&expected, collision)?
                    };
                    (expected == filename).then_some((started_at, sequence, collision))
                })
            })
    }

    fn segment_variant_identity(&self, base: &str, filename: &str) -> Option<(u64, usize)> {
        if filename == base {
            return Some((0, 0));
        }
        if let Some((without_collision, suffix)) = numeric_suffix(filename) {
            if let Ok(collision) = usize::try_from(suffix) {
                if collision_recording_filename(base, collision)? == filename {
                    return Some((0, collision));
                }
                if self.segment_naming == RecordingSegmentNaming::SafeUnique
                    && let Some((_, sequence)) = numeric_suffix(&without_collision)
                {
                    let sequenced = sequenced_recording_filename(base, sequence)?;
                    if collision_recording_filename(&sequenced, collision)? == filename {
                        return Some((sequence, collision));
                    }
                }
            }
            if self.segment_naming == RecordingSegmentNaming::SafeUnique
                && sequenced_recording_filename(base, suffix)? == filename
            {
                return Some((suffix, 0));
            }
        }
        None
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

fn bounded_segment_filename(
    base: &str,
    started_at: u64,
    sequence: u64,
    extension_start: Option<usize>,
) -> Option<String> {
    let insertion = if sequence != 0 {
        format!("-{sequence:06}")
    } else {
        String::default()
    };
    let maximum = MAX_RECORDING_FILENAME_BYTES.checked_sub(MAX_RECORDING_COLLISION_SUFFIX_BYTES)?;
    if base.len().checked_add(insertion.len())? <= maximum {
        return insert_before_extension(base, &insertion);
    }

    let (stem, extension) = extension_start.map_or((base, ""), |index| base.split_at(index));
    let marker = format!(
        "~{}~{started_at}~{sequence}",
        recording_name_hash(base.as_bytes())
    );
    let maximum_prefix = maximum.checked_sub(marker.len() + extension.len())?;
    let mut prefix_end = stem.len().min(maximum_prefix);
    while !stem.is_char_boundary(prefix_end) {
        prefix_end -= 1;
    }
    Some(format!("{}{marker}{extension}", &stem[..prefix_end]))
}

fn recording_name_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest
        .iter()
        .fold(String::with_capacity(64), |mut hash, byte| {
            write!(hash, "{byte:02X}").expect("writing to a String cannot fail");
            hash
        })
}

fn compact_identity_candidates(filename: &str) -> impl Iterator<Item = (u64, u64)> + '_ {
    filename.match_indices('~').filter_map(|(marker_start, _)| {
        let hash_start = marker_start.checked_add(1)?;
        let hash_end = hash_start.checked_add(64)?;
        let bytes = filename.as_bytes();
        if bytes.get(hash_end) != Some(&b'~')
            || !bytes
                .get(hash_start..hash_end)?
                .iter()
                .all(u8::is_ascii_hexdigit)
        {
            return None;
        }
        let started_at_start = hash_end + 1;
        let started_at_end = bytes[started_at_start..]
            .iter()
            .position(|byte| *byte == b'~')?
            .checked_add(started_at_start)?;
        let sequence_start = started_at_end + 1;
        let sequence_length = bytes[sequence_start..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if sequence_length == 0 {
            return None;
        }
        Some((
            filename[started_at_start..started_at_end].parse().ok()?,
            filename[sequence_start..sequence_start + sequence_length]
                .parse()
                .ok()?,
        ))
    })
}

fn numeric_suffix(name: &str) -> Option<(String, u64)> {
    let extension_start = name.rfind('.').filter(|index| *index > 0);
    let (stem, extension) = extension_start.map_or((name, ""), |index| name.split_at(index));
    let (stem, suffix) = stem.rsplit_once('-')?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((format!("{stem}{extension}"), suffix.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_safe_sequence_and_collision_variants_for_resume() {
        let policy = RecordingPathPolicy::new(".flv", true).expect("recording policy");

        assert_eq!(
            policy.segment_identity_from_filename(b"camera", "camera-100.flv"),
            Some((100, 0, 0))
        );
        assert_eq!(
            policy.segment_identity_from_filename(b"camera", "camera-100-2.flv"),
            Some((100, 0, 2))
        );
        assert_eq!(
            policy.segment_identity_from_filename(b"camera", "camera-100-000001.flv"),
            Some((100, 1, 0))
        );
        assert_eq!(
            policy.segment_identity_from_filename(b"camera", "camera-100-000001-2.flv"),
            Some((100, 1, 2))
        );
    }

    #[test]
    fn recognizes_only_collision_variants_for_nginx_naming() {
        let policy = RecordingPathPolicy::new(".flv", true)
            .expect("recording policy")
            .with_segment_policy(
                RecordingTimezone::Utc,
                RecordingTimeBasis::SegmentStart,
                RecordingSegmentNaming::NginxCompatible,
            );

        assert_eq!(
            policy.segment_identity_from_filename(b"camera", "camera-100-3.flv"),
            Some((100, 0, 3))
        );
        assert_eq!(
            policy.segment_identity_from_filename(b"camera", "camera-100-000001.flv"),
            None
        );
    }

    #[test]
    fn preserves_identity_when_suffixing_a_maximum_length_filename() {
        let policy = RecordingPathPolicy::new(".flv", true).expect("recording policy");
        let stream_name = vec![b'a'; 247];
        let base = policy
            .relative_filename_at(&stream_name, 100)
            .expect("maximum length base");
        assert_eq!(base.len(), MAX_RECORDING_FILENAME_BYTES);

        let sequenced = policy
            .segment_filename(
                &stream_name,
                100,
                RecordingDateTime::from_unix_seconds(100).expect("date time"),
                1,
            )
            .expect("sequenced filename");
        assert!(sequenced.len() <= MAX_RECORDING_FILENAME_BYTES - 3);
        assert_eq!(
            policy.segment_identity_from_filename(&stream_name, &sequenced),
            Some((100, 1, 0))
        );

        let collided = collision_recording_filename(&sequenced, 15).expect("collision filename");
        assert_eq!(collided.len(), MAX_RECORDING_FILENAME_BYTES);
        assert_eq!(
            policy.segment_identity_from_filename(&stream_name, &collided),
            Some((100, 1, 15))
        );
    }

    #[test]
    fn compact_segment_identities_do_not_alias_streams_or_long_suffixes() {
        let policy = RecordingPathPolicy::new(&format!("{}.flv", "x".repeat(70)), true)
            .expect("recording policy");
        let mut first_stream = vec![b'a'; 177];
        let mut second_stream = first_stream.clone();
        first_stream[80] = b'b';
        second_stream[80] = b'c';
        let opened_at = RecordingDateTime::from_unix_seconds(100).expect("date time");
        let first = policy
            .segment_filename(&first_stream, 100, opened_at, 1)
            .expect("first filename");
        let second = policy
            .segment_filename(&second_stream, 100, opened_at, 1)
            .expect("second filename");

        assert_ne!(first, second);
        assert_eq!(
            policy.segment_identity_from_filename(&first_stream, &first),
            Some((100, 1, 0))
        );
        assert_eq!(
            policy.segment_identity_from_filename(&second_stream, &first),
            None
        );
    }

    #[test]
    fn recognizes_native_seconds_before_a_digit_prefixed_suffix() {
        let policy = RecordingPathPolicy::new("1.flv", true).expect("recording policy");
        assert_eq!(
            policy.segment_identity_from_filename(b"camera", "camera-1001.flv"),
            Some((100, 0, 0))
        );
    }

    #[test]
    fn compact_identity_distinguishes_a_numeric_suffix_from_a_collision() {
        let policy = RecordingPathPolicy::new("-2.flv", true).expect("recording policy");
        let stream_name = vec![b'a'; 245];
        let filename = policy
            .segment_filename(
                &stream_name,
                100,
                RecordingDateTime::from_unix_seconds(100).expect("date time"),
                1,
            )
            .expect("compact filename");
        assert_eq!(
            policy.segment_identity_from_filename(&stream_name, &filename),
            Some((100, 1, 0))
        );

        let collision = collision_recording_filename(&filename, 2).expect("collision filename");
        assert_eq!(
            policy.segment_identity_from_filename(&stream_name, &collision),
            Some((100, 1, 2))
        );
    }
}
