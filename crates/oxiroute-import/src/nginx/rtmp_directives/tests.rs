use super::{valid_duration, valid_seconds_duration, valid_size};

#[test]
fn validates_nginx_size_and_time_literals() {
    assert!(valid_size("0"));
    assert!(valid_size("1K"));
    assert!(valid_size("4m"));
    assert!(!valid_size("1T"));

    assert!(valid_duration("0"));
    assert!(valid_duration("250ms"));
    assert!(valid_duration("1h30m"));
    assert!(!valid_duration("1m2h"));
    assert!(!valid_duration("1ms2s"));
    assert!(!valid_duration("1M"));
    assert!(valid_seconds_duration("1M2w"));
    assert!(!valid_seconds_duration("1ms"));
    assert!(!valid_duration("soon"));
}
