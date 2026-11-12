//! The indexed [`RouteTable`] must select exactly what a scan over every route selects.

use super::*;

impl HostMatcher {
    fn rank_if_matches(&self, host: Option<&str>) -> Option<(u8, usize)> {
        match self {
            Self::Any => Some((0, 0)),
            Self::ExactAuthority(expected) => host
                .is_some_and(|host| host == expected)
                .then_some((3, expected.len())),
            Self::AsciiCaseInsensitiveExactAuthority(expected) => host
                .is_some_and(|host| host.eq_ignore_ascii_case(expected))
                .then_some((3, expected.len())),
            Self::NormalizedExact(expected) => host
                .and_then(normalized_authority_host)
                .is_some_and(|host| host == *expected)
                .then_some((2, expected.len())),
            Self::Wildcard(suffix) => host
                .and_then(normalized_authority_host)
                .is_some_and(|host| wildcard_matches(&host, suffix))
                .then_some((1, suffix.len())),
            Self::NginxLeadingWildcard(suffix) => host
                .and_then(normalized_authority_host)
                .is_some_and(|host| nginx_suffix_matches(&host, suffix, false))
                .then_some((1, suffix.len())),
            Self::NginxLeadingDot(suffix) => host
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
    let host = authority.or_else(|| uri.authority()).map(Authority::as_str);
    let path = canonicalize_http_path(uri.path())?;
    let mut best = None;
    for route in &table.routes {
        if !route.matches_method(method) {
            continue;
        }
        let Some(host_rank) = route.host.rank_if_matches(host) else {
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
                let expected =
                    select_linear(table, Some(&authority), &uri, &method).map(Route::route_id);
                assert_eq!(
                    table
                        .select(Some(&authority), &uri, &method)
                        .map(Route::route_id),
                    expected,
                    "{authority} {target} {method}"
                );
                let order = table.select_index(Some(authority.as_str()), uri.path(), &method);
                assert_eq!(
                    order.map(|order| table.routes[order].route_id()),
                    expected,
                    "index {authority} {target} {method}"
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

fn reference_is_dns_name(host: &str) -> bool {
    !host.is_empty()
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
}

fn reference_parse_host_ip(host: &str) -> Option<IpAddr> {
    host.parse::<IpAddr>().ok().or_else(|| {
        host.strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .and_then(|host| host.parse::<IpAddr>().ok())
            .filter(IpAddr::is_ipv6)
    })
}

fn walk(alphabet: &[u8], left: usize, text: &mut Vec<u8>, check: &mut impl FnMut(&str)) {
    check(std::str::from_utf8(text).expect("ascii"));
    if left == 0 {
        return;
    }
    for &byte in alphabet {
        text.push(byte);
        walk(alphabet, left - 1, text, check);
        text.pop();
    }
}

fn every_string(alphabet: &[u8], length: usize, check: &mut impl FnMut(&str)) {
    walk(alphabet, length, &mut Vec::new(), check);
}

#[test]
fn the_single_pass_dns_name_check_matches_the_label_by_label_check() {
    every_string(b"a-.1_", 8, &mut |host| {
        assert_eq!(is_dns_name(host), reference_is_dns_name(host), "{host:?}");
    });
    for length in [62, 63, 64, 65] {
        let label = "a".repeat(length);
        for host in [
            label.clone(),
            format!("{label}.{label}"),
            format!("{label}-"),
            format!("-{label}"),
        ] {
            assert_eq!(is_dns_name(&host), reference_is_dns_name(&host), "{host:?}");
        }
    }
}

#[test]
fn only_hosts_that_can_be_ip_literals_are_parsed_as_ip_addresses() {
    every_string(b"1.:[]af", 7, &mut |host| {
        assert_eq!(
            parse_host_ip(host),
            reference_parse_host_ip(host),
            "{host:?}"
        );
    });
    for host in [
        "1.2.3.4",
        "[::1]",
        "::ffff:1.2.3.4",
        "[1.2.3.4]",
        "fe80::1",
        "2001:db8::1",
        "example.com",
    ] {
        assert_eq!(
            parse_host_ip(host),
            reference_parse_host_ip(host),
            "{host:?}"
        );
    }
}

#[test]
fn the_host_of_an_authority_is_what_http_reports() {
    for text in [
        "a.example.com",
        "A.Example.com:8080",
        "[::1]",
        "[::1]:8080",
        "u@a.example.com",
        "u:p@a.example.com:81",
        "192.0.2.254:80",
        "a.example.com.",
        "a_b.example.com:1",
        "a:1",
    ] {
        let authority: Authority = text.parse().expect("authority");
        assert_eq!(host_of(text), authority.host(), "{text}");
    }
}

#[test]
fn the_host_of_any_text_is_what_http_reports_when_the_text_is_an_authority() {
    let mut authorities = 0_u32;
    every_string(b"a1.:[]@%-", 6, &mut |text| {
        let host = host_of(text);
        if let Ok(authority) = text.parse::<Authority>() {
            authorities += 1;
            assert_eq!(host, authority.host(), "{text:?}");
        }
    });
    assert!(authorities > 10_000, "{authorities}");
}

#[test]
fn a_path_with_no_single_meaning_selects_no_route() {
    let table = table_of(vec![None]);
    for path in [
        "/a/../b", "/a/./b", "//a", "/a\\b", "/a%2fb", "/a%zz", "/%41",
    ] {
        assert_eq!(table.select_index(None, path, &Method::GET), None, "{path}");
    }
    assert_eq!(table.select_index(None, "/a%3fb", &Method::GET), Some(0));
}

fn table_of(hosts: Vec<Option<HttpHostSelector>>) -> RouteTable {
    let routes = hosts
        .into_iter()
        .enumerate()
        .map(|(index, host)| {
            let path = HttpPathSelector::SegmentPrefix { value: "/".into() };
            Route::new(host, path, None, format!("route-{index}")).expect("route")
        })
        .collect();
    RouteTable::new(routes)
}

#[test]
fn a_table_of_catch_all_hosts_does_not_select_on_the_host() {
    assert!(!RouteTable::default().selects_on_host());
    assert!(!table_of(vec![None, None]).selects_on_host());
}

#[test]
fn a_route_bound_to_a_host_makes_the_table_select_on_the_host() {
    let exact = |value: &str| HttpHostSelector::ExactAuthority {
        value: value.into(),
    };
    let normalized = |value: &str| HttpHostSelector::NormalizedHost {
        value: value.into(),
    };
    let folded = |value: &str| HttpHostSelector::AsciiCaseInsensitiveExactAuthority {
        value: value.into(),
    };
    let leading = |value: &str| HttpHostSelector::NginxLeadingWildcard {
        value: value.into(),
    };
    for host in [
        exact("a.example.com"),
        folded("a.example.com"),
        normalized("a.example.com"),
        normalized("*.example.com"),
        leading("example.com"),
    ] {
        assert!(
            table_of(vec![None, Some(host.clone())]).selects_on_host(),
            "{host:?}"
        );
    }
}
