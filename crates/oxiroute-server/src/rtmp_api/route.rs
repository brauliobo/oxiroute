pub(super) fn parse_four_segments<'a>(path: &'a str, prefix: &str) -> Option<[&'a str; 4]> {
    let value = path.strip_prefix(prefix)?;
    let mut segments = value.splitn(4, '/');
    let segments = [
        segments.next()?,
        segments.next()?,
        segments.next()?,
        segments.next()?,
    ];
    segments
        .iter()
        .all(|segment| !segment.is_empty())
        .then_some(segments)
}

#[cfg(test)]
mod tests {
    use super::parse_four_segments;

    #[test]
    fn preserves_slashes_in_the_fourth_segment() {
        assert_eq!(
            parse_four_segments("/prefix/a/b/c/d/e", "/prefix/"),
            Some(["a", "b", "c", "d/e"])
        );
    }

    #[test]
    fn rejects_wrong_prefixes_and_empty_segments() {
        assert!(parse_four_segments("/other/a/b/c/d", "/prefix/").is_none());
        assert!(parse_four_segments("/prefix/a//c/d", "/prefix/").is_none());
    }
}
