use serde::Serializer;

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `serialize_with` callback receives `&T`.
pub(crate) fn serialize_u64_string<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&value.to_string())
}
