pub(super) struct Route<'a> {
    pub(super) service: &'a str,
    pub(super) application: &'a str,
    pub(super) source: &'a str,
    pub(super) path: &'a str,
}

pub(super) fn match_route(path: &str) -> Option<Route<'_>> {
    let [service, application, source, path] =
        super::route::parse_four_segments(path, "/api/v1/rtmp/vod/")?;
    Some(Route {
        service,
        application,
        source,
        path,
    })
}

const MAX_TICKETED_PATH_BYTES: usize = 1_024;

pub(super) fn is_ticketable_path(path: &str) -> bool {
    path.len() <= MAX_TICKETED_PATH_BYTES
        && path.split('/').all(|segment| {
            !matches!(segment, "" | "." | "..")
                && segment.bytes().all(|byte| {
                    byte.is_ascii_graphic() && !matches!(byte, b'?' | b'#' | b'%' | b'\\')
                })
        })
}

#[cfg(test)]
mod tests {
    use super::is_ticketable_path;

    #[test]
    fn ticketable_paths_are_clean_relative_object_paths() {
        assert!(is_ticketable_path("camera/2026/clip-1.flv"));
        for path in [
            "",
            "/clip.flv",
            "a//b.flv",
            "../clip.flv",
            "a/./b",
            "a?b",
            "a#b",
            "a%20b",
            "a b",
        ] {
            assert!(!is_ticketable_path(path), "{path}");
        }
        assert!(!is_ticketable_path(&"a".repeat(1_025)));
    }
}
