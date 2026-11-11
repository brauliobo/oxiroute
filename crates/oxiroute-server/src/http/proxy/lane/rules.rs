//! Header rules of a route on the lane. A route compiles to the lane only when its rules are ones the head codec can
//! apply as edits; the values come from the classic policy functions, fed with the scanned head instead of a header
//! map, so the two paths cannot drift.

use std::net::IpAddr;

use http::{HeaderName, HeaderValue, uri::Authority};
use oxiroute_h1_head::{Class, Edits, EditsFull, Name, RequestHead, Value};

use crate::http::{
    action::{
        ProxyPolicyPlan, RequestHeaderMutationPlan, RequestHeaderValuePlan,
        ResponseHeaderMutationPlan,
    },
    policy::{
        IncomingHeaders, RequestHeaderDecision, RequestPolicyContext, RequestPolicyError,
        decide_request_header, join_values,
    },
};

/// Rules a route may have, request and response each: the codec's edit list holds [`oxiroute_h1_head::MAX_EDITS`] and
/// the fixed edits of a head take about half of it.
const MAX_RULES: usize = 12;

/// The rules a request head is edited by, for the request at hand.
pub(super) type Decisions = Vec<RequestHeaderDecision>;

/// Whether every rule of the policy is one the lane applies.
pub(super) fn supported(policy: &ProxyPolicyPlan) -> bool {
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

/// What the rules of a request read besides its headers.
pub(super) struct Source<'a> {
    pub(super) buffer: &'a [u8],
    pub(super) head: &'a RequestHead,
    pub(super) authority: &'a Authority,
    pub(super) client_ip: Option<IpAddr>,
    pub(super) https: bool,
}

struct Scanned<'a>(&'a Source<'a>);

impl IncomingHeaders for Scanned<'_> {
    fn joined(
        &self,
        name: &HeaderName,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, RequestPolicyError> {
        let (buffer, name) = (self.0.buffer, name.as_str().as_bytes());
        let values = self
            .0
            .head
            .headers()
            .iter()
            .filter(|header| header.name(buffer).eq_ignore_ascii_case(name))
            .map(|header| header.value(buffer));
        join_values(values, max_bytes)
    }
}

/// The decisions the classic request filter takes for the request, in the order of the rules. An error is a request
/// the classic path answers itself (a source too large, a client address it cannot name): the caller hands it over.
pub(super) fn request_decisions(
    policy: &ProxyPolicyPlan,
    source: &Source<'_>,
) -> Result<Decisions, RequestPolicyError> {
    let context = RequestPolicyContext {
        authority: Some(source.authority),
        downstream_scheme: if source.https { "https" } else { "http" },
        client_ip: source.client_ip,
        incoming_headers: &Scanned(source),
    };
    let mut decisions = Vec::with_capacity(policy.request_headers.len());
    for rule in policy
        .request_headers
        .iter()
        .filter(|rule| !rule.is_pingora_managed_upgrade())
    {
        let decision = decide_request_header(rule, context)?;
        // Resolved at write time, where the host the request goes with is known; the one value that cannot be is a
        // client address the listener has none of.
        if let RequestHeaderDecision::Set { value, .. } = &decision {
            value.resolve(None).or_else(|error| match error {
                RequestPolicyError::SelectedUpstreamHostUnavailable => Ok(None),
                error => Err(error),
            })?;
        }
        decisions.push(decision);
    }
    Ok(decisions)
}

/// Appends the request rules to the edits of the request head. `host` is what the request is sent to the origin as,
/// when a rule can name it.
pub(super) fn edit_request<'a>(
    edits: &mut Edits<'a>,
    decisions: &'a [RequestHeaderDecision],
    host: Option<&'a HeaderValue>,
) -> Result<(), EditsFull> {
    for decision in decisions {
        match decision {
            RequestHeaderDecision::Remove(name) => edits.remove(lane_name(name))?,
            RequestHeaderDecision::Set { name, value } => {
                let value = value
                    .resolve(host)
                    .expect("rules were resolved when they were taken");
                if let Some(value) = value {
                    edits.set(lane_name(name), Value::trusted(value.as_bytes()))?;
                }
            }
        }
    }
    Ok(())
}

/// Appends the response rules that apply to a response with `status` to the edits of its head.
pub(super) fn edit_response<'a>(
    edits: &mut Edits<'a>,
    policy: &'a ProxyPolicyPlan,
    status: u16,
) -> Result<(), EditsFull> {
    for rule in policy
        .response_headers
        .iter()
        .filter(|rule| rule.applies(status))
    {
        match rule {
            ResponseHeaderMutationPlan::Set { name, value, .. } => {
                edits.set(lane_name(name), Value::trusted(value.as_bytes()))?;
            }
            ResponseHeaderMutationPlan::Add { name, value, .. } => {
                edits.add(lane_name(name), Value::trusted(value.as_bytes()))?;
            }
            ResponseHeaderMutationPlan::Remove { name } => edits.remove(lane_name(name))?,
        }
    }
    Ok(())
}

fn lane_name(name: &HeaderName) -> Name<'_> {
    Name::new(name.as_str().as_bytes())
        .expect("the route compiled to the lane with supported names")
}

#[cfg(test)]
mod tests {
    use oxiroute_config::{
        HttpProxyPolicy, HttpRequestHeaderMutation, HttpRequestHeaderValue,
        HttpResponseHeaderMutation,
    };
    use oxiroute_h1_head::{Scan, scan_request};

    use super::*;

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
        let decisions = request_decisions(&policy, &source).expect("decisions");
        let mut edits = Edits::new();
        edit_request(&mut edits, &decisions, None).expect("edits");
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
            request_decisions(&policy, &source).err(),
            Some(RequestPolicyError::SourceTooLarge)
        );
    }
}
