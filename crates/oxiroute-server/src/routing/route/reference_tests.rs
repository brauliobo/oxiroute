//! The indexed [`RouteTable`] must select exactly what a scan over every route selects.

use super::*;

impl HostMatcher {
    fn rank_if_matches(&self, authority: Option<&Authority>) -> Option<(u8, usize)> {
        match self {
            Self::Any => Some((0, 0)),
            Self::ExactAuthority(expected) => authority
                .is_some_and(|authority| authority.as_str() == expected)
                .then_some((3, expected.len())),
            Self::AsciiCaseInsensitiveExactAuthority(expected) => authority
                .is_some_and(|authority| authority.as_str().eq_ignore_ascii_case(expected))
                .then_some((3, expected.len())),
            Self::NormalizedExact(expected) => authority
                .and_then(normalized_authority_host)
                .is_some_and(|host| host == *expected)
                .then_some((2, expected.len())),
            Self::Wildcard(suffix) => authority
                .and_then(normalized_authority_host)
                .is_some_and(|host| wildcard_matches(&host, suffix))
                .then_some((1, suffix.len())),
            Self::NginxLeadingWildcard(suffix) => authority
                .and_then(normalized_authority_host)
                .is_some_and(|host| nginx_suffix_matches(&host, suffix, false))
                .then_some((1, suffix.len())),
            Self::NginxLeadingDot(suffix) => authority
                .and_then(normalized_authority_host)
                .is_some_and(|host| nginx_suffix_matches(&host, suffix, true))
                .then_some((1, suffix.len())),
        }
    }
}

fn wildcard_matches(host: &str, suffix: &str) -> bool {
    let Some(suffix_start) = host.len().checked_sub(suffix.len()) else {
        return false;
    };
    let Some(dot_index) = suffix_start.checked_sub(1) else {
        return false;
    };

    host.as_bytes().get(dot_index) == Some(&b'.')
        && host[suffix_start..].eq_ignore_ascii_case(suffix)
        && dot_index > 0
        && !host[..dot_index].contains('.')
}

fn nginx_suffix_matches(host: &str, suffix: &str, include_base: bool) -> bool {
    if include_base && host == suffix {
        return true;
    }
    host.len() > suffix.len()
        && host.ends_with(suffix)
        && host
            .as_bytes()
            .get(host.len() - suffix.len() - 1)
            .is_some_and(|separator| *separator == b'.')
}

/// The original selection: visit every route, keep the greatest score, earliest route on ties.
fn select_linear<'a>(
    table: &'a RouteTable,
    authority: Option<&Authority>,
    uri: &Uri,
    method: &Method,
) -> Option<&'a Route> {
    let authority = authority.or_else(|| uri.authority());
    let path = canonicalize_http_path(uri.path())?;
    let mut best = None;
    for route in &table.routes {
        if !route.matches_method(method) {
            continue;
        }
        let Some(host_rank) = route.host.rank_if_matches(authority) else {
            continue;
        };
        let Some((path_rank, path_length)) = route.path.rank_if_matches(path.as_ref()) else {
            continue;
        };
        let score = (
            host_rank,
            path_rank,
            path_length,
            u8::from(route.methods.is_some()),
        );
        match best {
            Some((_, best_score)) if best_score >= score => {}
            _ => best = Some((route, score)),
        }
    }
    best.map(|(route, _)| route)
}

struct Random(u64);

impl Random {
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % bound as u64).expect("bounded")
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

fn hosts() -> Vec<Option<HttpHostSelector>> {
    let exact = |value: &str| HttpHostSelector::ExactAuthority {
        value: value.into(),
    };
    let folded = |value: &str| HttpHostSelector::AsciiCaseInsensitiveExactAuthority {
        value: value.into(),
    };
    let normalized = |value: &str| HttpHostSelector::NormalizedHost {
        value: value.into(),
    };
    let leading = |value: &str| HttpHostSelector::NginxLeadingWildcard {
        value: value.into(),
    };
    let dotted = |value: &str| HttpHostSelector::NginxLeadingDot {
        value: value.into(),
    };
    vec![
        None,
        None,
        Some(exact("a.example.com")),
        Some(exact("a.example.com:8080")),
        Some(exact("A.Example.com")),
        Some(exact("192.0.2.254:80")),
        Some(exact("[::1]:8080")),
        Some(folded("a.example.com")),
        Some(folded("B.example.com:8080")),
        Some(normalized("a.example.com")),
        Some(normalized("b.example.com")),
        Some(normalized("example.com")),
        Some(normalized("*.example.com")),
        Some(normalized("*.a.example.com")),
        Some(normalized("*.2.3.4")),
        Some(normalized("192.0.2.254")),
        Some(normalized("[::1]")),
        Some(leading("example.com")),
        Some(leading("a.example.com")),
        Some(dotted("example.com")),
        Some(dotted("a.example.com")),
        Some(dotted("com")),
    ]
}

const AUTHORITIES: &[&str] = &[
    "a.example.com",
    "A.EXAMPLE.COM",
    "a.example.com:8080",
    "b.example.com",
    "B.Example.com:8080",
    "x.a.example.com",
    "x.y.a.example.com",
    "example.com",
    "example.com.",
    "com",
    "192.0.2.254",
    "192.0.2.254:80",
    "1.2.3.4",
    "[::1]:8080",
    "[::1]",
    "unknown.org",
    "under_score.example.com",
];

fn paths() -> Vec<HttpPathSelector> {
    let segment = |value: &str| HttpPathSelector::SegmentPrefix {
        value: value.into(),
    };
    let raw = |value: &str| HttpPathSelector::RawPrefix {
        value: value.into(),
    };
    let exact = |value: &str| HttpPathSelector::Exact {
        value: value.into(),
    };
    let folded = |value: &str| HttpPathSelector::AsciiCaseInsensitiveExact {
        value: value.into(),
    };
    vec![
        segment("/"),
        segment("/"),
        segment("/api"),
        segment("/api/"),
        segment("/api/v1"),
        segment("/a"),
        segment("/x%2A"),
        raw("/"),
        raw("/api"),
        raw("/ap"),
        raw("/api/v"),
        exact("/"),
        exact("/api"),
        exact("/api/v1/items"),
        folded("/API"),
        folded("/Api/V1"),
    ]
}

const TARGETS: &[&str] = &[
    "/",
    "/api",
    "/api/",
    "/api/v1",
    "/api/v1/items",
    "/api/v1/items/7",
    "/apix",
    "/API",
    "/ap",
    "/a",
    "/a/b",
    "/x%2a",
    "/x%2A/y",
    "/api/V1",
    "/zzz",
];

fn methods() -> Vec<Option<Vec<Method>>> {
    vec![
        None,
        None,
        Some(vec![Method::GET]),
        Some(vec![Method::POST]),
        Some(vec![Method::GET, Method::POST]),
    ]
}

fn random_table(random: &mut Random, size: usize) -> RouteTable {
    let (hosts, paths, methods) = (hosts(), paths(), methods());
    let routes = (0..size)
        .map(|id| {
            Route::new(
                random.pick(&hosts).clone(),
                random.pick(&paths).clone(),
                random.pick(&methods).clone(),
                format!("route-{id}"),
            )
            .expect("route")
        })
        .collect();
    RouteTable::new(routes)
}

fn assert_equivalent(table: &RouteTable) {
    for authority in AUTHORITIES
        .iter()
        .map(|authority| authority.parse::<Authority>().expect("authority"))
    {
        for target in TARGETS {
            let uri: Uri = target.parse().expect("uri");
            for method in [Method::GET, Method::POST, Method::PUT] {
                assert_eq!(
                    table
                        .select(Some(&authority), &uri, &method)
                        .map(Route::route_id),
                    select_linear(table, Some(&authority), &uri, &method).map(Route::route_id),
                    "{authority} {target} {method}"
                );
            }
        }
    }
    for target in TARGETS {
        let uri: Uri = target.parse().expect("uri");
        assert_eq!(
            table.select(None, &uri, &Method::GET).map(Route::route_id),
            select_linear(table, None, &uri, &Method::GET).map(Route::route_id),
            "no authority {target}"
        );
    }
}

#[test]
fn small_tables_select_like_a_scan_of_every_route() {
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    for _ in 0..150 {
        let size = 1 + random.below(12);
        assert_equivalent(&random_table(&mut random, size));
    }
}

#[test]
fn large_tables_select_like_a_scan_of_every_route() {
    let mut random = Random(0xD1B5_4A32_D192_ED03);
    for _ in 0..60 {
        let size = 20 + random.below(100);
        assert_equivalent(&random_table(&mut random, size));
    }
}

#[test]
fn an_authority_in_the_uri_is_used_when_the_request_has_none() {
    let route = |host, id: &str| {
        Route::new(
            Some(HttpHostSelector::NormalizedHost { value: host }),
            HttpPathSelector::SegmentPrefix { value: "/".into() },
            None,
            id,
        )
        .expect("route")
    };
    let table = RouteTable::new(vec![
        route("a.example.com".into(), "a"),
        route("b.example.com".into(), "b"),
    ]);
    let uri: Uri = "http://b.example.com/x".parse().expect("uri");
    assert_eq!(
        table.select(None, &uri, &Method::GET).map(Route::route_id),
        Some("b")
    );
}
