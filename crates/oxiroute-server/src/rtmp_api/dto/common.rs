use serde::{Serialize, Serializer};

#[derive(Serialize)]
pub(crate) struct ErrorResponse {
    error: ErrorBody,
}

impl ErrorResponse {
    pub(crate) fn new(code: &'static str, message: String) -> Self {
        Self {
            error: ErrorBody { code, message },
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DecimalCounter(u64);

impl From<u64> for DecimalCounter {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl Serialize for DecimalCounter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::DecimalCounter;

    #[test]
    fn decimal_counter_preserves_the_maximum_u64() {
        assert_eq!(
            serde_json::to_string(&DecimalCounter::from(u64::MAX)).expect("counter JSON"),
            r#""18446744073709551615""#
        );
    }
}
