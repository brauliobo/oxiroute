//! Header rules of a route on the lane. A route compiles to the lane only when its rules are ones the head codec can
//! apply as edits. What is the same for every request is worked out once, when the route's generation is built (the
//! codec's name of each header, the literal values, which rules a status applies); what depends on the request is
//! rendered by the classic policy function (`render_request_value`), fed with the scanned head instead of a header
//! map, so the two paths cannot drift.

use std::net::IpAddr;

use http::{HeaderName, HeaderValue, uri::Authority};
use oxiroute_h1_head::{Class, Edits, EditsFull, Name, RequestHead, Value};

use super::{LaneRoute, log::RouteLog};
use crate::http::{
    action::{
        HttpActionPlan, ProxyPolicyPlan, RequestHeaderMutationPlan, RequestHeaderValuePlan,
        ResponseHeaderMutationPlan,
    },
    policy::{
        IncomingHeaders, Rendered, RequestPolicyContext, RequestPolicyError, join_values_into,
        render_request_value,
    },
};
use crate::label::Label;

/// Rules a route may have, request and response each: the codec's edit list holds [`oxiroute_h1_head::MAX_EDITS`] and
/// the fixed edits of a head take about half of it.
const MAX_RULES: usize = 12;

/// Whether every rule of the policy is one the lane applies.
pub(in crate::http::proxy) fn supported(policy: &ProxyPolicyPlan) -> bool {
    let request = policy
        .request_headers
        .iter()
        .filter(|rule| !rule.is_pingora_managed_upgrade());
    let mut count = 0;
    for rule in request {
        count += 1;
        if !request_rule_supported(rule) {
            return false;
        }
    }
    count <= MAX_RULES
        && policy.response_headers.len() <= MAX_RULES
        && policy
            .response_headers
            .iter()
            .all(|rule| name_supported(response_rule_name(rule)))
}

/// A name the codec writes, and not one of the two the lane stamps itself: a rule on the request id would see the
/// id the classic path stamped, which the lane stamps later.
fn name_supported(name: &HeaderName) -> bool {
    Name::new(name.as_str().as_bytes())
        .is_some_and(|name| !matches!(name.class(), Class::XRequestId | Class::XUpstreamRequestId))
}

fn request_rule_supported(rule: &RequestHeaderMutationPlan) -> bool {
    match rule {
        RequestHeaderMutationPlan::Remove { name } => name_supported(name),
        RequestHeaderMutationPlan::Set { name, value } => {
            name_supported(name)
                && match value {
                    RequestHeaderValuePlan::IncomingHeader { name: source, .. } => {
                        name_supported(source)
                    }
                    _ => true,
                }
        }
    }
}

fn response_rule_name(rule: &ResponseHeaderMutationPlan) -> &HeaderName {
    match rule {
        ResponseHeaderMutationPlan::Set { name, .. }
        | ResponseHeaderMutationPlan::Add { name, .. }
        | ResponseHeaderMutationPlan::Remove { name } => name,
    }
}

/// A rule of the route's policy, with the codec's class of the header it edits.
#[derive(Clone, Copy, Debug)]
struct Rule {
    /// Position in the policy's list.
    index: usize,
    class: Class,
}

impl Rule {
    fn new(index: usize, name: &HeaderName) -> Self {
        let class = Name::new(name.as_str().as_bytes())
            .expect("the route compiled to the lane with supported names")
            .class();
        Self { index, class }
    }

    fn name<'a>(&self, name: &'a HeaderName) -> Name<'a> {
        Name::with_class(name.as_str().as_bytes(), self.class)
    }
}

/// The rules of a route that the lane applies, as positions in its policy: what is decided once per generation.
#[derive(Debug, Default)]
pub(crate) struct RouteEdits {
    request: Box<[Rule]>,
    response: Box<[Rule]>,
    /// Some request rule takes its value from the request.
    computed: bool,
    /// What the route's access lines repeat.
    pub(in crate::http::proxy) log: RouteLog,
}

impl RouteEdits {
    /// Whether a request rule has a value to work out for the request.
    pub(in crate::http::proxy) const fn computes_request_values(&self) -> bool {
        self.computed
    }

    /// The edits of a route the lane proxies; none for any other.
    pub(crate) fn compile(lane: LaneRoute, action: &HttpActionPlan, route_id: &Label) -> Self {
        match (lane, action) {
            (LaneRoute::Proxy, HttpActionPlan::Proxy(proxy)) => Self {
                log: RouteLog::new(Some(proxy.pool.label()), route_id),
                ..Self::of(&proxy.policy)
            },
            (LaneRoute::Direct, _) => Self {
                log: RouteLog::new(None, route_id),
                ..Self::default()
            },
            _ => Self::default(),
        }
    }

    fn of(policy: &ProxyPolicyPlan) -> Self {
        let request: Box<[Rule]> = policy
            .request_headers
            .iter()
            .enumerate()
            .filter(|(_, rule)| !rule.is_pingora_managed_upgrade())
            .map(|(index, rule)| match rule {
                RequestHeaderMutationPlan::Set { name, .. }
                | RequestHeaderMutationPlan::Remove { name } => Rule::new(index, name),
            })
            .collect();
        let response = policy
            .response_headers
            .iter()
            .enumerate()
            .map(|(index, rule)| Rule::new(index, response_rule_name(rule)))
            .collect();
        let computed = request.iter().any(|rule| {
            matches!(
                &policy.request_headers[rule.index],
                RequestHeaderMutationPlan::Set { value, .. }
                    if !matches!(value, RequestHeaderValuePlan::Literal(_))
            )
        });
        Self {
            request,
            response,
            computed,
            log: RouteLog::default(),
        }
    }
}

/// What the rules of a request produced: the value bytes of the rules that compute one.
#[derive(Debug, Default)]
pub(in crate::http::proxy) struct Evaluated {
    bytes: Vec<u8>,
    slots: [Slot; MAX_RULES],
    /// A rule names the host the request is sent to the origin with.
    selected_host: bool,
}

#[derive(Clone, Copy, Debug, Default)]
enum Slot {
    /// A rule that removes, has a literal or leaves the header as it is.
    #[default]
    None,
    Bytes(u32, u32),
    SelectedHost,
}

impl Evaluated {
    pub(in crate::http::proxy) fn into_buffer(self) -> Vec<u8> {
        self.bytes
    }

    /// Whether a rule names the host the request is sent with.
    pub(in crate::http::proxy) const fn names_host(&self) -> bool {
        self.selected_host
    }
}

/// What the rules of a request read besides its headers.
pub(in crate::http::proxy) struct Source<'a> {
    pub(in crate::http::proxy) buffer: &'a [u8],
    pub(in crate::http::proxy) head: &'a RequestHead,
    pub(in crate::http::proxy) authority: &'a Authority,
    pub(in crate::http::proxy) client_ip: Option<IpAddr>,
    pub(in crate::http::proxy) https: bool,
}

struct Scanned<'a>(&'a Source<'a>);

impl IncomingHeaders for Scanned<'_> {
    fn join_into(
        &self,
        name: &HeaderName,
        max_bytes: usize,
        out: &mut Vec<u8>,
    ) -> Result<bool, RequestPolicyError> {
        let (buffer, name) = (self.0.buffer, name.as_str().as_bytes());
        let values = self
            .0
            .head
            .headers()
            .iter()
            .filter(|header| header.name(buffer).eq_ignore_ascii_case(name))
            .map(|header| header.value(buffer));
        join_values_into(out, values, max_bytes)
    }
}

/// The values the classic request filter computes for the request, in the order of the rules, written to `bytes`. An
/// error is a request the classic path answers itself (a source too large, a client address it cannot name): the
/// caller hands it over.
pub(in crate::http::proxy) fn evaluate(
    edits: &RouteEdits,
    policy: &ProxyPolicyPlan,
    source: &Source<'_>,
    bytes: Vec<u8>,
) -> Result<Evaluated, RequestPolicyError> {
    let mut evaluated = Evaluated {
        bytes,
        ..Evaluated::default()
    };
    let context = RequestPolicyContext {
        authority: Some(source.authority),
        downstream_scheme: if source.https { "https" } else { "http" },
        client_ip: source.client_ip,
        incoming_headers: &Scanned(source),
    };
    for (rule, slot) in edits.request.iter().zip(&mut evaluated.slots) {
        let RequestHeaderMutationPlan::Set { value, .. } = &policy.request_headers[rule.index]
        else {
            continue;
        };
        if matches!(value, RequestHeaderValuePlan::Literal(_)) {
            continue;
        }
        let start = evaluated.bytes.len();
        *slot = match render_request_value(value, context, &mut evaluated.bytes)? {
            Rendered::Value | Rendered::ClientIp | Rendered::ForwardedFor => {
                // The text comes from an authority and from header values the scanner checked, and the rest is
                // addresses and literals: nothing here is a byte a header value refuses.
                debug_assert!(Value::new(&evaluated.bytes[start..]).is_some());
                let len = evaluated.bytes.len() - start;
                Slot::Bytes(
                    u32::try_from(start).expect("scratch bytes are bounded"),
                    u32::try_from(len).expect("a value is bounded"),
                )
            }
            Rendered::ClientIpMissing => return Err(RequestPolicyError::ClientIpUnavailable),
            Rendered::Preserve | Rendered::NoOp => Slot::None,
            Rendered::SelectedUpstreamHost => {
                evaluated.selected_host = true;
                Slot::SelectedHost
            }
        };
    }
    Ok(evaluated)
}

/// Appends the request rules to the edits of the request head. `host` is what the request is sent to the origin as,
/// when a rule can name it.
pub(in crate::http::proxy) fn edit_request<'a>(
    out: &mut Edits<'a>,
    edits: &RouteEdits,
    policy: &'a ProxyPolicyPlan,
    evaluated: &'a Evaluated,
    host: Option<&'a HeaderValue>,
) -> Result<(), EditsFull> {
    for (rule, slot) in edits.request.iter().zip(&evaluated.slots) {
        match &policy.request_headers[rule.index] {
            RequestHeaderMutationPlan::Remove { name } => out.remove(rule.name(name))?,
            RequestHeaderMutationPlan::Set { name, value } => {
                let value = match (value, slot) {
                    (RequestHeaderValuePlan::Literal(value), _) => value.as_bytes(),
                    (_, Slot::Bytes(start, len)) => {
                        &evaluated.bytes[*start as usize..(*start + *len) as usize]
                    }
                    (_, Slot::SelectedHost) => host
                        .expect("a rule naming the upstream host was given one")
                        .as_bytes(),
                    (_, Slot::None) => continue,
                };
                out.set(rule.name(name), Value::trusted(value))?;
            }
        }
    }
    Ok(())
}

/// Appends the response rules that apply to a response with `status` to the edits of its head.
pub(in crate::http::proxy) fn edit_response<'a>(
    out: &mut Edits<'a>,
    edits: &RouteEdits,
    policy: &'a ProxyPolicyPlan,
    status: u16,
) -> Result<(), EditsFull> {
    for rule in &edits.response {
        let plan = &policy.response_headers[rule.index];
        if !plan.applies(status) {
            continue;
        }
        match plan {
            ResponseHeaderMutationPlan::Set { name, value, .. } => {
                out.set(rule.name(name), Value::trusted(value.as_bytes()))?;
            }
            ResponseHeaderMutationPlan::Add { name, value, .. } => {
                out.add(rule.name(name), Value::trusted(value.as_bytes()))?;
            }
            ResponseHeaderMutationPlan::Remove { name } => out.remove(rule.name(name))?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use oxiroute_config::{
        HttpProxyPolicy, HttpRequestHeaderMutation, HttpRequestHeaderValue,
        HttpResponseHeaderMutation,
    };
    use oxiroute_h1_head::{Scan, scan_request};

    use super::*;
    use crate::http::policy::RequestHeaderDecision;

    fn set(name: &str, value: HttpRequestHeaderValue) -> HttpRequestHeaderMutation {
        HttpRequestHeaderMutation::Set {
            name: name.into(),
            value,
        }
    }

    fn plan(request_headers: Vec<HttpRequestHeaderMutation>) -> ProxyPolicyPlan {
        ProxyPolicyPlan::compile(&HttpProxyPolicy {
            request_headers,
            ..HttpProxyPolicy::default()
        })
    }

    #[test]
    fn forwarding_rules_are_supported() {
        let rules = vec![
            set(
                "x-forwarded-for",
                HttpRequestHeaderValue::AppendedXForwardedFor {
                    max_bytes: 256,
                    except_source_cidrs: Vec::new(),
                },
            ),
            set("x-real-ip", HttpRequestHeaderValue::ClientIp),
        ];
        assert!(supported(&plan(rules)));
    }

    #[test]
    fn rules_on_the_request_id_or_too_many_rules_stay_classic() {
        let literal = || HttpRequestHeaderValue::Literal { value: "v".into() };
        assert!(!supported(&plan(vec![set("x-request-id", literal())])));
        let many = (0..=MAX_RULES)
            .map(|index| set(&format!("x-r{index}"), literal()))
            .collect();
        assert!(!supported(&plan(many)));
        let responses = (0..=MAX_RULES)
            .map(|index| HttpResponseHeaderMutation::Remove {
                name: format!("x-r{index}"),
            })
            .collect();
        let policy = ProxyPolicyPlan::compile(&HttpProxyPolicy {
            response_headers: responses,
            ..HttpProxyPolicy::default()
        });
        assert!(!supported(&policy));
    }

    #[test]
    fn the_forwarded_for_chain_is_read_from_the_scanned_head() {
        let buffer = b"GET / HTTP/1.1\r\nHost: a.test\r\nX-Forwarded-For: 203.0.113.5\r\nx-forwarded-for: 198.51.100.7\r\n\r\n";
        let mut head = RequestHead::new();
        assert_eq!(scan_request(buffer, &mut head), Scan::Head);
        let authority: Authority = "a.test".parse().unwrap();
        let source = Source {
            buffer,
            head: &head,
            authority: &authority,
            client_ip: Some("192.0.2.1".parse().unwrap()),
            https: false,
        };
        let policy = plan(vec![set(
            "x-forwarded-for",
            HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes: 256,
                except_source_cidrs: Vec::new(),
            },
        )]);
        let route = RouteEdits::of(&policy);
        let evaluated = evaluate(&route, &policy, &source, Vec::new()).expect("values");
        let mut edits = Edits::new();
        edit_request(&mut edits, &route, &policy, &evaluated, None).expect("edits");
        let mut out = Vec::new();
        oxiroute_h1_head::write_request(&mut out, buffer, &head, &edits);
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("x-forwarded-for: 203.0.113.5, 198.51.100.7, 192.0.2.1\r\n"),
            "{text}"
        );
        assert_eq!(text.matches("x-forwarded-for").count(), 1, "{text}");
    }

    #[test]
    fn a_source_too_large_is_left_to_the_classic_path() {
        let long = "1".repeat(300);
        let buffer = format!("GET / HTTP/1.1\r\nHost: a.test\r\nX-Forwarded-For: {long}\r\n\r\n");
        let mut head = RequestHead::new();
        assert_eq!(scan_request(buffer.as_bytes(), &mut head), Scan::Head);
        let authority: Authority = "a.test".parse().unwrap();
        let source = Source {
            buffer: buffer.as_bytes(),
            head: &head,
            authority: &authority,
            client_ip: None,
            https: false,
        };
        let policy = plan(vec![set(
            "x-forwarded-for",
            HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes: 256,
                except_source_cidrs: Vec::new(),
            },
        )]);
        assert_eq!(
            evaluate(&RouteEdits::of(&policy), &policy, &source, Vec::new()).err(),
            Some(RequestPolicyError::SourceTooLarge)
        );
    }

    /// Every kind of value, for clients of every shape: what the lane writes is what the classic decision completes.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn every_kind_of_value_is_what_the_classic_decision_gives() {
        let kinds = [
            HttpRequestHeaderValue::Literal {
                value: "fixed".into(),
            },
            HttpRequestHeaderValue::IncomingAuthority,
            HttpRequestHeaderValue::NormalizedHost,
            HttpRequestHeaderValue::NginxHost {
                fallback: "fallback.test".into(),
            },
            HttpRequestHeaderValue::ClientIp,
            HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes: 64,
                except_source_cidrs: vec!["198.51.100.0/24".into()],
            },
            HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes: 20,
                except_source_cidrs: Vec::new(),
            },
            HttpRequestHeaderValue::DownstreamScheme,
            HttpRequestHeaderValue::IncomingHeader {
                name: "x-a".into(),
                max_bytes: 16,
            },
            HttpRequestHeaderValue::IncomingHeader {
                name: "x-a".into(),
                max_bytes: 4,
            },
            HttpRequestHeaderValue::IncomingHeader {
                name: "x-none".into(),
                max_bytes: 4,
            },
            HttpRequestHeaderValue::SelectedUpstreamHost,
        ];
        let buffer = b"GET / HTTP/1.1\r\nHost: Client.Example.:8443\r\nX-Forwarded-For: 203.0.113.5\r\nx-forwarded-for: 198.51.100.7\r\nX-A: one\r\nx-a: two\r\n\r\n";
        let mut head = RequestHead::new();
        assert_eq!(scan_request(buffer, &mut head), Scan::Head);
        let mut map = http::HeaderMap::new();
        for (name, value) in [
            ("x-forwarded-for", "203.0.113.5"),
            ("x-forwarded-for", "198.51.100.7"),
            ("x-a", "one"),
            ("x-a", "two"),
        ] {
            map.append(name, HeaderValue::from_static(value));
        }
        let selected = HeaderValue::from_static("origin.test:9");
        for authority in ["Client.Example.:8443", "[2001:db8::1]:80", "203.0.113.9"] {
            let authority: Authority = authority.parse().unwrap();
            for client_ip in [
                Some("192.0.2.1"),
                Some("198.51.100.3"),
                Some("2001:db8::1"),
                None,
            ] {
                let client_ip: Option<IpAddr> = client_ip.map(|ip| ip.parse().unwrap());
                for https in [false, true] {
                    let source = Source {
                        buffer,
                        head: &head,
                        authority: &authority,
                        client_ip,
                        https,
                    };
                    for kind in &kinds {
                        let policy = plan(vec![set("x-rule", kind.clone())]);
                        let context = RequestPolicyContext {
                            authority: Some(&authority),
                            downstream_scheme: if https { "https" } else { "http" },
                            client_ip,
                            incoming_headers: &map,
                        };
                        let classic = crate::http::policy::decide_request_header(
                            &policy.request_headers[0],
                            context,
                        )
                        .and_then(|decision| match decision {
                            RequestHeaderDecision::Set { value, .. } => {
                                value.complete(Some(&selected))
                            }
                            RequestHeaderDecision::Remove(_) => Ok(None),
                        });
                        let route = RouteEdits::of(&policy);
                        let lane = evaluate(&route, &policy, &source, Vec::new()).map(|values| {
                            let mut edits = Edits::new();
                            edit_request(&mut edits, &route, &policy, &values, Some(&selected))
                                .expect("edits");
                            edits.as_slice().first().map(|edit| match edit {
                                oxiroute_h1_head::Edit::Set(_, value) => value.as_bytes().to_vec(),
                                other => panic!("{other:?}"),
                            })
                        });
                        let case = format!("{kind:?} {authority} {client_ip:?} {https}");
                        match (classic, lane) {
                            (Ok(classic), Ok(lane)) => assert_eq!(
                                classic.as_ref().map(HeaderValue::as_bytes),
                                lane.as_deref(),
                                "{case}"
                            ),
                            (classic, lane) => {
                                assert_eq!(classic.is_err(), lane.is_err(), "{case}");
                            }
                        }
                    }
                }
            }
        }
    }
}
