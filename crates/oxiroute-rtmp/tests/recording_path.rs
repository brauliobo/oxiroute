use std::path::Path;

use oxiroute_rtmp::{
    MAX_RECORDING_FILENAME_BYTES, MAX_RECORDING_SUFFIX_TEMPLATE_BYTES, RecordingDateTime,
    RecordingPathError, RecordingPathPolicy,
};

#[test]
fn escapes_the_complete_stream_name_as_one_component_without_reparsing_queries() {
    let policy = policy(".flv", false);

    assert_eq!(
        render(&policy, b"app/camera?token=a/b"),
        "app%2Fcamera%3Ftoken%3Da%2Fb.flv"
    );
    assert_eq!(
        render(&policy, b"camera??ignored"),
        "camera%3F%3Fignored.flv"
    );
    assert!(matches!(
        policy.relative_filename(b"camera?token=\0", 1_721_657_969, opened_at()),
        Err(RecordingPathError::NulInStreamName)
    ));
    assert_eq!(render(&policy, b"cam%era"), "cam%25era.flv");
    assert_eq!(
        render(&policy, b"../room/..\\feed"),
        "%2E.%2Froom%2F..%5Cfeed.flv"
    );

    let absolute_looking = render(&policy, b"/var/stream");
    assert_eq!(absolute_looking, "%2Fvar%2Fstream.flv");
    assert!(Path::new(&absolute_looking).is_relative());
    assert_eq!(Path::new(&absolute_looking).components().count(), 1);

    let hidden_looking = render(&policy, b".camera");
    assert_eq!(hidden_looking, "%2Ecamera.flv");
    assert!(Path::new(&hidden_looking).is_relative());
    assert_eq!(Path::new(&hidden_looking).components().count(), 1);
}

#[test]
fn rejects_empty_dot_dotdot_and_nul_stream_names() {
    let policy = policy(".flv", false);

    for stream_name in [b"".as_slice(), b".", b".."] {
        assert!(matches!(
            policy.relative_filename(stream_name, 1_721_657_969, opened_at()),
            Err(RecordingPathError::InvalidStreamName)
        ));
    }
    assert!(matches!(
        policy.relative_filename(b"cam\0era", 1_721_657_969, opened_at()),
        Err(RecordingPathError::NulInStreamName)
    ));
    assert_eq!(render(&policy, b"?token=x"), "%3Ftoken%3Dx.flv");
}

#[test]
fn percent_escapes_utf8_non_utf8_and_every_unsafe_byte_deterministically() {
    let policy = policy("", false);
    assert_eq!(
        render(&policy, "café/直播".as_bytes()),
        "caf%C3%A9%2F%E7%9B%B4%E6%92%AD"
    );
    assert_eq!(render(&policy, &[0xff, 0x80, b'%', b' ']), "%FF%80%25%20");

    for byte in 1_u8..=u8::MAX {
        let stream_name = [b'x', byte];
        let expected = if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
        {
            format!("x{}", char::from(byte))
        } else {
            format!("x%{byte:02X}")
        };
        assert_eq!(render(&policy, &stream_name), expected, "byte {byte:#04x}");
    }
}

#[test]
fn renders_only_the_audited_utc_tokens_and_literal_percent() {
    let policy = policy("-%Y-%m-%dT%H-%M-%S-%%.flv", false);
    assert_eq!(
        render(&policy, b"camera"),
        "camera-2024-07-22T13-26-09-%.flv"
    );
}

#[test]
fn validates_suffix_templates_by_bytes_separators_nul_and_format_items() {
    let exactly_128_bytes = "é".repeat(MAX_RECORDING_SUFFIX_TEMPLATE_BYTES / 2);
    assert!(RecordingPathPolicy::new(&exactly_128_bytes, false).is_ok());

    let too_long = format!("{exactly_128_bytes}x");
    assert!(matches!(
        RecordingPathPolicy::new(&too_long, false),
        Err(RecordingPathError::SuffixTemplateTooLong { length })
            if length == MAX_RECORDING_SUFFIX_TEMPLATE_BYTES + 1
    ));
    for template in ["/.flv", "\\.flv"] {
        assert!(matches!(
            RecordingPathPolicy::new(template, false),
            Err(RecordingPathError::SuffixTemplateContainsSeparator { .. })
        ));
    }
    assert!(matches!(
        RecordingPathPolicy::new(".flv\0", false),
        Err(RecordingPathError::NulInSuffixTemplate)
    ));
    assert!(matches!(
        RecordingPathPolicy::new("%", false),
        Err(RecordingPathError::InvalidSuffixFormat { .. })
    ));
    for item in 1_u8..=0x7f {
        if matches!(item, b'Y' | b'm' | b'd' | b'H' | b'M' | b'S' | b'%') {
            continue;
        }
        let template = format!("%{}", char::from(item));
        let result = RecordingPathPolicy::new(&template, false);
        if matches!(item, b'/' | b'\\') {
            assert!(matches!(
                result,
                Err(RecordingPathError::SuffixTemplateContainsSeparator { .. })
            ));
        } else {
            assert!(matches!(
                result,
                Err(RecordingPathError::InvalidSuffixFormat { .. })
            ));
        }
    }
    for template in ["%é", "%直播"] {
        assert!(matches!(
            RecordingPathPolicy::new(template, false),
            Err(RecordingPathError::InvalidSuffixFormat { .. })
        ));
    }
}

#[test]
fn native_unique_seconds_suffix_is_stable_but_not_collision_free() {
    let ordinary = policy("-%Y.flv", false);
    let native_unique = policy("-%Y.flv", true);

    assert_eq!(render(&ordinary, b"camera"), "camera-2024.flv");
    let first_open = render(&native_unique, b"camera");
    let second_open = render(&native_unique, b"camera");
    assert_eq!(first_open, "camera-1721657969-2024.flv");
    assert_eq!(
        first_open, second_open,
        "two opens in one Unix second still collide"
    );
}

#[test]
fn enforces_rendered_filename_byte_boundaries_after_escaping() {
    let no_suffix = policy("", false);
    let exactly_max = vec![b'a'; MAX_RECORDING_FILENAME_BYTES];
    assert_eq!(
        render(&no_suffix, &exactly_max).len(),
        MAX_RECORDING_FILENAME_BYTES
    );

    let too_many_safe_bytes = vec![b'a'; MAX_RECORDING_FILENAME_BYTES + 1];
    assert!(matches!(
        no_suffix.relative_filename(&too_many_safe_bytes, 1_721_657_969, opened_at()),
        Err(RecordingPathError::FilenameTooLong { length })
            if length == MAX_RECORDING_FILENAME_BYTES + 1
    ));

    let exactly_max_after_escaping = vec![0xff; MAX_RECORDING_FILENAME_BYTES / 3];
    assert_eq!(
        render(&no_suffix, &exactly_max_after_escaping).len(),
        MAX_RECORDING_FILENAME_BYTES
    );
    let too_many_escaped_bytes = vec![0xff; MAX_RECORDING_FILENAME_BYTES / 3 + 1];
    assert!(matches!(
        no_suffix.relative_filename(&too_many_escaped_bytes, 1_721_657_969, opened_at()),
        Err(RecordingPathError::FilenameTooLong { length })
            if length == MAX_RECORDING_FILENAME_BYTES + 3
    ));

    let suffix = policy(".flv", false);
    let base_at_suffix_boundary = vec![b'a'; MAX_RECORDING_FILENAME_BYTES - 4];
    assert_eq!(
        render(&suffix, &base_at_suffix_boundary).len(),
        MAX_RECORDING_FILENAME_BYTES
    );
    let base_over_suffix_boundary = vec![b'a'; MAX_RECORDING_FILENAME_BYTES - 3];
    assert!(matches!(
        suffix.relative_filename(&base_over_suffix_boundary, 1_721_657_969, opened_at()),
        Err(RecordingPathError::FilenameTooLong { length })
            if length == MAX_RECORDING_FILENAME_BYTES + 1
    ));
}

#[test]
fn validates_the_injected_utc_date_time_without_external_time_dependencies() {
    assert!(RecordingDateTime::new(0, 1, 1, 0, 0, 0).is_ok());
    assert!(RecordingDateTime::new(9_999, 12, 31, 23, 59, 59).is_ok());
    for result in [
        RecordingDateTime::new(10_000, 1, 1, 0, 0, 0),
        RecordingDateTime::new(2024, 13, 1, 0, 0, 0),
        RecordingDateTime::new(2023, 2, 29, 0, 0, 0),
        RecordingDateTime::new(2024, 1, 1, 24, 0, 0),
        RecordingDateTime::new(2024, 1, 1, 0, 60, 0),
        RecordingDateTime::new(2024, 1, 1, 0, 0, 60),
    ] {
        assert!(matches!(
            result,
            Err(RecordingPathError::InvalidDateTime { .. })
        ));
    }
    assert!(RecordingDateTime::new(2024, 2, 29, 0, 0, 0).is_ok());
}

#[test]
fn converts_unix_seconds_across_epoch_leap_and_supported_boundaries() {
    for (seconds, expected) in [
        (0, (1970, 1, 1, 0, 0, 0)),
        (86_399, (1970, 1, 1, 23, 59, 59)),
        (86_400, (1970, 1, 2, 0, 0, 0)),
        (951_782_399, (2000, 2, 28, 23, 59, 59)),
        (951_782_400, (2000, 2, 29, 0, 0, 0)),
        (951_868_800, (2000, 3, 1, 0, 0, 0)),
        (4_102_444_800, (2100, 1, 1, 0, 0, 0)),
        (253_402_300_799, (9999, 12, 31, 23, 59, 59)),
    ] {
        let actual = RecordingDateTime::from_unix_seconds(seconds).expect("supported Unix time");
        let expected = RecordingDateTime::new(
            expected.0, expected.1, expected.2, expected.3, expected.4, expected.5,
        )
        .expect("expected date-time");
        assert_eq!(actual, expected, "Unix second {seconds}");
    }
    assert!(matches!(
        RecordingDateTime::from_unix_seconds(253_402_300_800),
        Err(RecordingPathError::InvalidDateTime {
            field: "year",
            value: 10_000
        })
    ));
    assert!(RecordingDateTime::from_unix_seconds(u64::MAX).is_err());
}

#[test]
fn unix_conversion_is_exhaustive_at_every_gregorian_day_boundary() {
    let mut seconds = 0_u64;
    for year in 1970..=9_999 {
        for month in 1..=12 {
            let mut day = 1;
            loop {
                let Ok(expected) = RecordingDateTime::new(year, month, day, 0, 0, 0) else {
                    break;
                };
                assert_eq!(
                    RecordingDateTime::from_unix_seconds(seconds).expect("supported day"),
                    expected,
                    "{year:04}-{month:02}-{day:02}"
                );
                seconds += 86_400;
                day += 1;
            }
        }
    }
    assert_eq!(seconds, 253_402_300_800);
}

fn policy(suffix_template: &str, native_unique_seconds: bool) -> RecordingPathPolicy {
    RecordingPathPolicy::new(suffix_template, native_unique_seconds)
        .expect("valid recording path policy")
}

fn opened_at() -> RecordingDateTime {
    RecordingDateTime::new(2024, 7, 22, 13, 26, 9).expect("valid date-time")
}

fn render(policy: &RecordingPathPolicy, stream_name: &[u8]) -> String {
    policy
        .relative_filename(stream_name, 1_721_657_969, opened_at())
        .expect("valid relative filename")
}
