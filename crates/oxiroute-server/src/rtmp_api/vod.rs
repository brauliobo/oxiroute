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
