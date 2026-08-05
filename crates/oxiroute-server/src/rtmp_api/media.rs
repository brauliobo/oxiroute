pub(super) struct Route<'a> {
    pub(super) service: &'a str,
    pub(super) application: &'a str,
    pub(super) stream: &'a str,
    pub(super) object: &'a str,
}

pub(super) fn match_route(path: &str) -> Option<Route<'_>> {
    let value = path.strip_prefix("/api/v1/rtmp/media/")?;
    let mut segments = value.splitn(4, '/');
    let service = segments.next()?;
    let application = segments.next()?;
    let stream = segments.next()?;
    let object = segments.next()?;
    if service.is_empty() || application.is_empty() || stream.is_empty() || object.is_empty() {
        return None;
    }
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
