pub(super) struct Route<'a> {
    pub(super) service: &'a str,
    pub(super) application: &'a str,
    pub(super) stream: &'a str,
    pub(super) object: &'a str,
}

pub(super) fn match_route(path: &str) -> Option<Route<'_>> {
    let [service, application, stream, object] =
        super::route::parse_four_segments(path, "/api/v1/rtmp/media/")?;
    Some(Route {
        service,
        application,
        stream,
        object,
    })
}

#[cfg(test)]
mod tests {
    use super::match_route;

    #[test]
    fn preserves_nested_media_object_paths() {
        let route = match_route("/api/v1/rtmp/media/live/camera/stream/main/keys/key-7.bin")
            .expect("media route");
        assert_eq!(route.service, "live");
        assert_eq!(route.application, "camera");
        assert_eq!(route.stream, "stream");
        assert_eq!(route.object, "main/keys/key-7.bin");
    }

    #[test]
    fn rejects_incomplete_media_paths() {
        assert!(match_route("/api/v1/rtmp/media/live/camera/stream").is_none());
    }
}
