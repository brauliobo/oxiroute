use crate::cli::top::units;

#[test]
fn counts_and_rates_scale_to_short_readable_text() {
    assert_eq!(units::count(0), "0");
    assert_eq!(units::count(999), "999");
    assert_eq!(units::count(1_000), "1.0k");
    assert_eq!(units::count(15_400), "15k");
    assert_eq!(units::count(2_500_000), "2.5M");
    assert_eq!(units::rate(0.0), "0");
    assert_eq!(units::rate(0.26), "0.3");
    assert_eq!(units::rate(42.0), "42");
    assert_eq!(units::bytes_rate(4_000_000.0), "3.8M");
    assert_eq!(units::bytes(1_500), "1.5K");
}

#[test]
fn durations_read_in_the_largest_fitting_units() {
    assert_eq!(units::millis(850), "850ms");
    assert_eq!(units::millis(1_500), "1.5s");
    assert_eq!(units::millis(12_000), "12s");
    assert_eq!(units::duration(125_000), "2m05s");
    assert_eq!(units::duration(7_500_000), "2h05m");
    assert_eq!(units::duration(97_200_000), "1d03h");
}

#[test]
fn a_percentage_needs_a_denominator() {
    assert_eq!(units::percent(1, 0), None);
    assert_eq!(units::percent(3, 4), Some(75.0));
}
