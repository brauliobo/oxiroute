use serde::Serializer;

/// The decimal digits of `value`, written at the end of `digits`.
pub(crate) fn decimal(value: u64, digits: &mut [u8; 20]) -> &str {
    let mut start = digits.len();
    let mut rest = value;
    loop {
        start -= 1;
        digits[start] = b'0' + u8::try_from(rest % 10).expect("a digit fits u8");
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    std::str::from_utf8(&digits[start..]).expect("ASCII digits")
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `serialize_with` callback receives `&T`.
pub(crate) fn serialize_u64_string<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(decimal(*value, &mut [0; 20]))
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    #[derive(Serialize)]
    struct Counter(#[serde(serialize_with = "super::serialize_u64_string")] u64);

    #[test]
    fn counters_are_decimal_strings_over_the_whole_u64_range() {
        for value in [
            0,
            7,
            10,
            99,
            1_000,
            18_446_744_073_709_551_615,
            u64::MAX / 3,
        ] {
            assert_eq!(
                serde_json::to_string(&Counter(value)).unwrap(),
                format!("\"{value}\"")
            );
        }
    }
}
