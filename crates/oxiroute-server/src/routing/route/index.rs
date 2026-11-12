use std::{
    cmp::Reverse,
    collections::HashMap,
    hash::{BuildHasherDefault, Hasher},
};

use http::Method;

use super::{HostMatcher, PathIndex, Route, normalized_authority_host};

/// How a matching route compares with the others: host, then path, then method specificity, then
/// source order. The greatest rank wins.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Rank {
    host: (u8, usize),
    path: (u8, usize),
    method: u8,
    order: Reverse<u32>,
}

impl Rank {
    pub(super) fn new(path: (u8, usize), route: &Route, order: u32) -> Self {
        Self {
            host: (0, 0),
            path,
            method: u8::from(route.methods.is_some()),
            order: Reverse(order),
        }
    }

    pub(super) fn on_host(mut self, host: (u8, usize)) -> Self {
        self.host = host;
        self
    }

    pub(super) fn order(self) -> usize {
        self.order.0 as usize
    }
}

/// FNV-1a. The keys come from the configuration, never from requests, so the table cannot be
/// flooded with colliding keys, and a request key is hashed in a few instructions per byte.
#[derive(Default)]
pub(super) struct Fnv(u64);

impl Hasher for Fnv {
    fn write(&mut self, bytes: &[u8]) {
        let seed = if self.0 == 0 {
            0xcbf2_9ce4_8422_2325
        } else {
            self.0
        };
        self.0 = bytes.iter().fold(seed, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        });
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

pub(super) type FnvMap<V> = HashMap<Box<str>, V, BuildHasherDefault<Fnv>>;

type HostMap = FnvMap<PathIndex>;

/// Routes grouped by host selector so a request only visits routes whose host can match.
///
/// Each group is keyed by the exact string its selector can match (the authority, its lowercase
/// form, the normalized host or the label-stripped suffix of a wildcard), so a lookup costs one
/// probe per candidate key instead of one comparison per route.
#[derive(Clone, Debug, Default)]
pub(super) struct RouteIndex {
    any: Option<PathIndex>,
    exact: HostMap,
    ascii_ci: HostMap,
    normalized: HostMap,
    wildcard: HostMap,
    nginx_wildcard: HostMap,
    nginx_dot: HostMap,
}

#[derive(Default)]
struct Groups {
    any: Vec<u32>,
    exact: FnvMap<Vec<u32>>,
    ascii_ci: FnvMap<Vec<u32>>,
    normalized: FnvMap<Vec<u32>>,
    wildcard: FnvMap<Vec<u32>>,
    nginx_wildcard: FnvMap<Vec<u32>>,
    nginx_dot: FnvMap<Vec<u32>>,
}

fn paths(groups: FnvMap<Vec<u32>>, routes: &[Route]) -> HostMap {
    groups
        .into_iter()
        .map(|(host, members)| (host, PathIndex::new(&members, routes)))
        .collect()
}

impl RouteIndex {
    pub(super) fn new(routes: &[Route]) -> Self {
        let mut groups = Groups::default();
        for (order, route) in routes.iter().enumerate() {
            let order = u32::try_from(order).expect("route count fits u32");
            match &route.host {
                HostMatcher::Any => groups.any.push(order),
                HostMatcher::ExactAuthority(host) => {
                    groups
                        .exact
                        .entry(host.as_str().into())
                        .or_default()
                        .push(order);
                }
                HostMatcher::AsciiCaseInsensitiveExactAuthority(host) => groups
                    .ascii_ci
                    .entry(host.to_ascii_lowercase().into())
                    .or_default()
                    .push(order),
                HostMatcher::NormalizedExact(host) => {
                    groups
                        .normalized
                        .entry(host.as_str().into())
                        .or_default()
                        .push(order);
                }
                HostMatcher::Wildcard(suffix) => {
                    groups
                        .wildcard
                        .entry(suffix.as_str().into())
                        .or_default()
                        .push(order);
                }
                HostMatcher::NginxLeadingWildcard(suffix) => {
                    groups
                        .nginx_wildcard
                        .entry(suffix.as_str().into())
                        .or_default()
                        .push(order);
                }
                HostMatcher::NginxLeadingDot(suffix) => {
                    groups
                        .nginx_dot
                        .entry(suffix.as_str().into())
                        .or_default()
                        .push(order);
                }
            }
        }
        Self {
            any: (!groups.any.is_empty()).then(|| PathIndex::new(&groups.any, routes)),
            exact: paths(groups.exact, routes),
            ascii_ci: paths(groups.ascii_ci, routes),
            normalized: paths(groups.normalized, routes),
            wildcard: paths(groups.wildcard, routes),
            nginx_wildcard: paths(groups.nginx_wildcard, routes),
            nginx_dot: paths(groups.nginx_dot, routes),
        }
    }

    /// The best route for the request. Host classes are visited from the most to the least
    /// specific, and the first class with a route that also matches path and method decides.
    pub(super) fn best(
        &self,
        routes: &[Route],
        authority: Option<&str>,
        path: &str,
        method: &Method,
    ) -> Option<Rank> {
        let found = authority.and_then(|authority| self.best_for(routes, authority, path, method));
        found.or_else(|| {
            self.any
                .as_ref()
                .and_then(|any| any.best(routes, path, method))
        })
    }

    fn best_for(
        &self,
        routes: &[Route],
        written: &str,
        path: &str,
        method: &Method,
    ) -> Option<Rank> {
        let probe = |map: &HostMap, key: &str, class: u8| {
            map.get(key).and_then(|paths| {
                paths
                    .best(routes, path, method)
                    .map(|rank| rank.on_host((class, key.len())))
            })
        };
        let mut best = probe(&self.exact, written, 3);
        if !self.ascii_ci.is_empty() {
            best = best.max(probe(&self.ascii_ci, &written.to_ascii_lowercase(), 3));
        }
        if best.is_some() || !self.has_normalized_hosts() {
            return best;
        }
        let host = normalized_authority_host(written)?;
        if let found @ Some(_) = probe(&self.normalized, &host, 2) {
            return found;
        }
        let first_dot = host.find('.').filter(|&dot| dot > 0);
        best = first_dot.and_then(|dot| probe(&self.wildcard, &host[dot + 1..], 1));
        if self.nginx_wildcard.is_empty() && self.nginx_dot.is_empty() {
            return best;
        }
        best = best.max(probe(&self.nginx_dot, &host, 1));
        for dot in host.match_indices('.').map(|(dot, _)| dot) {
            let suffix = &host[dot + 1..];
            best = best.max(probe(&self.nginx_wildcard, suffix, 1)).max(probe(
                &self.nginx_dot,
                suffix,
                1,
            ));
        }
        best
    }

    /// Whether any route is bound to a host, so that the request authority can change the selection.
    pub(super) fn selects_on_host(&self) -> bool {
        self.has_normalized_hosts() || !(self.exact.is_empty() && self.ascii_ci.is_empty())
    }

    fn has_normalized_hosts(&self) -> bool {
        !(self.normalized.is_empty()
            && self.wildcard.is_empty()
            && self.nginx_wildcard.is_empty()
            && self.nginx_dot.is_empty())
    }
}
