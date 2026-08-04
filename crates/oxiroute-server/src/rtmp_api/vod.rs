pub(super) struct Route<'a> {
    pub(super) service: &'a str,
    pub(super) application: &'a str,
    pub(super) source: &'a str,
    pub(super) path: &'a str,
}

pub(super) fn match_route(path: &str) -> Option<Route<'_>> {
    let value = path.strip_prefix("/api/v1/rtmp/vod/")?;
    let mut segments = value.splitn(4, '/');
    let service = segments.next()?;
    let application = segments.next()?;
    let source = segments.next()?;
    let path = segments.next()?;
    if service.is_empty() || application.is_empty() || source.is_empty() || path.is_empty() {
        return None;
    }
    Some(Route {
        service,
        application,
        source,
        path,
    })
}
