use http::Method;

use super::{
    PathMatcher, Route,
    index::{FnvMap, Rank},
};

/// Groups with at most this many routes are checked in precedence order; larger ones use a trie.
const LINEAR_LIMIT: usize = 8;

/// The path and method side of the routes that share one host group.
#[derive(Clone, Debug)]
pub(super) enum PathIndex {
    /// Every route in precedence order, so the first one that matches wins.
    Ordered(Box<[u32]>),
    Trie(Box<PathTrie>),
}

impl PathIndex {
    pub(super) fn new(members: &[u32], routes: &[Route]) -> Self {
        if members.len() <= LINEAR_LIMIT {
            let mut ordered = members.to_vec();
            ordered.sort_by_key(|&order| {
                let route = &routes[order as usize];
                let (rank, length) = route.path.precedence();
                (
                    std::cmp::Reverse((rank, length, u8::from(route.methods.is_some()))),
                    order,
                )
            });
            Self::Ordered(ordered.into())
        } else {
            Self::Trie(Box::new(PathTrie::new(members, routes)))
        }
    }

    pub(super) fn best(&self, routes: &[Route], path: &str, method: &Method) -> Option<Rank> {
        match self {
            Self::Ordered(ordered) => ordered.iter().find_map(|&order| {
                let route = &routes[order as usize];
                let rank = route.path.rank_if_matches(path)?;
                route
                    .matches_method(method)
                    .then(|| Rank::new(rank, route, order))
            }),
            Self::Trie(trie) => trie.best(routes, path, method),
        }
    }
}

/// Routes sharing one path value: the method-specific ones first, then in source order.
type Members = Vec<u32>;

const RAW: usize = 0;
const SEGMENT: usize = 1;
const EXACT: usize = 2;

#[derive(Clone, Debug, Default)]
struct Node {
    edges: Vec<(u8, u32)>,
    /// Raw prefix, segment prefix and exact routes that end at this node.
    terminals: Option<Box<[Members; 3]>>,
}

impl Node {
    fn child(&self, byte: u8) -> Option<usize> {
        let found = if self.edges.len() <= 8 {
            self.edges.iter().find(|(edge, _)| *edge == byte)
        } else {
            self.edges
                .binary_search_by_key(&byte, |(edge, _)| *edge)
                .ok()
                .map(|at| &self.edges[at])
        };
        found.map(|(_, child)| *child as usize)
    }
}

/// A byte trie over the path values of one host group. Walking a request path visits every
/// value that is a prefix of it, which covers raw prefixes, segment prefixes and exact paths.
#[derive(Clone, Debug, Default)]
pub(super) struct PathTrie {
    nodes: Vec<Node>,
    /// The `/` segment prefix matches every path, including those that do not start with `/`.
    root: Members,
    /// ASCII case-insensitive exact paths by lowercase value.
    folded: FnvMap<Members>,
}

impl PathTrie {
    fn new(members: &[u32], routes: &[Route]) -> Self {
        let mut trie = Self {
            nodes: vec![Node::default()],
            ..Self::default()
        };
        for &order in members {
            match &routes[order as usize].path {
                PathMatcher::SegmentPrefix(value) if value == "/" => trie.root.push(order),
                PathMatcher::AsciiCaseInsensitiveExact(value) => trie
                    .folded
                    .entry(value.to_ascii_lowercase().into())
                    .or_default()
                    .push(order),
                PathMatcher::RawPrefix(value) => trie.insert(value, RAW, order),
                PathMatcher::SegmentPrefix(value) => trie.insert(value, SEGMENT, order),
                PathMatcher::Exact(value) => trie.insert(value, EXACT, order),
            }
        }
        let terminals = trie
            .nodes
            .iter_mut()
            .filter_map(|node| node.terminals.as_deref_mut());
        for members in terminals.flatten().chain(trie.folded.values_mut()) {
            specific_first(members, routes);
        }
        specific_first(&mut trie.root, routes);
        trie
    }

    fn insert(&mut self, value: &str, kind: usize, order: u32) {
        let node = value
            .bytes()
            .fold(0, |node, byte| self.child_or_insert(node, byte));
        self.nodes[node].terminals.get_or_insert_default()[kind].push(order);
    }

    fn child_or_insert(&mut self, node: usize, byte: u8) -> usize {
        match self.nodes[node]
            .edges
            .binary_search_by_key(&byte, |(edge, _)| *edge)
        {
            Ok(at) => self.nodes[node].edges[at].1 as usize,
            Err(at) => {
                let child = self.nodes.len();
                self.nodes.push(Node::default());
                let edge = (byte, u32::try_from(child).expect("trie fits u32"));
                self.nodes[node].edges.insert(at, edge);
                child
            }
        }
    }

    fn best(&self, routes: &[Route], path: &str, method: &Method) -> Option<Rank> {
        let bytes = path.as_bytes();
        let mut best = first_match(&self.root, routes, method, (1, 1));
        if !self.folded.is_empty() {
            let folded = self.folded.get(path.to_ascii_lowercase().as_str());
            let exact =
                folded.and_then(|members| first_match(members, routes, method, (2, path.len())));
            best = best.max(exact);
        }
        let (mut node, mut depth) = (0, 0);
        loop {
            if let Some(terminals) = &self.nodes[node].terminals {
                let at_end = depth == bytes.len();
                let on_boundary = at_end || bytes[depth] == b'/' || bytes[depth - 1] == b'/';
                best = best.max(first_match(&terminals[RAW], routes, method, (0, depth)));
                if on_boundary {
                    best = best.max(first_match(&terminals[SEGMENT], routes, method, (1, depth)));
                }
                if at_end {
                    best = best.max(first_match(&terminals[EXACT], routes, method, (2, depth)));
                }
            }
            let Some(child) = bytes
                .get(depth)
                .and_then(|&byte| self.nodes[node].child(byte))
            else {
                return best;
            };
            node = child;
            depth += 1;
        }
    }
}

fn specific_first(members: &mut [u32], routes: &[Route]) {
    members.sort_by_key(|&order| (routes[order as usize].methods.is_none(), order));
}

fn first_match(
    members: &[u32],
    routes: &[Route],
    method: &Method,
    path: (u8, usize),
) -> Option<Rank> {
    members.iter().find_map(|&order| {
        let route = &routes[order as usize];
        route
            .matches_method(method)
            .then(|| Rank::new(path, route, order))
    })
}
