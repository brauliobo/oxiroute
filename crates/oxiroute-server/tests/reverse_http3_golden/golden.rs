//! Normalisation of observations, the fixture file and the readable replay diff.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Map, Value, json};

use crate::fixture;

static SCRUB: OnceLock<Vec<(String, &'static str)>> = OnceLock::new();

pub const RECORD_VARIABLE: &str = "OXIROUTE_INTERNAL_TEST_RECORD_GOLDEN";
pub const RECORD_CASES_VARIABLE: &str = "OXIROUTE_INTERNAL_TEST_RECORD_CASES";
const INLINE_BODY_BYTES: usize = 4096;
const COMPACT_LINE: usize = 140;

/// Names the addresses of the helper endpoints, which change from run to run.
pub fn set_scrub(replacements: Vec<(String, &'static str)>) {
    SCRUB.set(replacements).expect("scrub list is set once");
}

pub fn scrub(text: &str) -> String {
    SCRUB
        .get()
        .into_iter()
        .flatten()
        .fold(text.to_owned(), |text, (from, to)| {
            text.replace(from.as_str(), to)
        })
}

/// Lowercased headers sorted by name (values of one name keep their order); `date` carries no information
/// and `age` changes with the clock.
pub fn headers_json<'a>(headers: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Value {
    let mut pairs = headers
        .into_iter()
        .map(|(name, value)| {
            (
                name.to_ascii_lowercase(),
                scrub(&String::from_utf8_lossy(value)),
            )
        })
        .filter(|(name, _)| name != "date")
        .map(|(name, value)| {
            if name == "age" {
                (name, "<age>".into())
            } else {
                (name, value)
            }
        })
        .collect::<Vec<_>>();
    pairs.sort_by(|left, right| left.0.cmp(&right.0));
    Value::Array(
        pairs
            .into_iter()
            .map(|(name, value)| json!([name, value]))
            .collect(),
    )
}

pub fn body_json(body: &[u8]) -> Value {
    match std::str::from_utf8(body) {
        Ok(text) if body.len() <= INLINE_BODY_BYTES => json!(text),
        _ if body.len() <= INLINE_BODY_BYTES => json!({ "base64": STANDARD.encode(body) }),
        _ => json!({ "len": body.len(), "fnv1a64": format!("{:016x}", fnv1a(body)) }),
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mix = |hash: u64, byte: &u8| (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, mix)
}

/// A case whose outcome the RFCs leave open (or that the recorded implementation handled leniently).
///
/// On replay the recorded steps, every `accept` alternative and every step value outside `ignore` pass; the
/// access log lines of a tolerant case are not compared.
pub struct Tolerance {
    pub reason: String,
    /// `{ "step": index, <key>: <replacement> ... }` replaces keys of one recorded step (null removes one);
    /// `{ "steps": [...] }` replaces all steps.
    pub accept: Vec<Value>,
    /// Paths such as `steps[0].outcome.body` that are not compared.
    pub ignore: Vec<String>,
    /// A connection error code that RFC 9114 mandates for the case: accepted as the connection closing instead.
    pub mandated: Option<u64>,
}

impl Tolerance {
    pub fn new(reason: &str) -> Self {
        Self {
            reason: reason.into(),
            accept: Vec::new(),
            ignore: Vec::new(),
            mandated: None,
        }
    }

    /// The RFC-mandated connection error is accepted where the recorded implementation did not apply it.
    pub fn mandated(reason: &str, code: u64) -> Self {
        Self {
            mandated: Some(code),
            ..Self::new(reason)
        }
    }

    pub fn accept(mut self, entry: Value) -> Self {
        self.accept.push(entry);
        self
    }

    /// Accepts `outcome` in place of the recorded outcome of `step`.
    pub fn outcome(self, step: usize, outcome: Value) -> Self {
        self.accept(json!({ "step": step, "outcome": outcome }))
    }

    pub fn ignore(mut self, path: &str) -> Self {
        self.ignore.push(path.into());
        self
    }

    /// The alternatives that the mandated close implies for these recorded steps.
    pub fn with_steps(mut self, steps: &[Value]) -> Self {
        let Some(code) = self.mandated else {
            return self;
        };
        let closed = json!({ "closed": code });
        let first = &steps[0];
        let only_the_close = json!({ "request": first["request"], "outcome": { "connection_closed": code }, "connection_after": closed });
        self.accept.push(json!({ "steps": [only_the_close] }));
        if first["outcome"].get("status").is_some() {
            let mut answered = first.clone();
            answered["connection_after"] = closed;
            self.accept.push(json!({ "steps": [answered] }));
        }
        self
    }
}

pub struct Case {
    pub id: String,
    pub hosts: Vec<String>,
    pub steps: Vec<Value>,
    pub tolerance: Option<Tolerance>,
}

impl Case {
    fn to_value(&self, access_log: Vec<Value>) -> Value {
        let mut case = json!({ "id": self.id, "steps": self.steps, "access_log": access_log });
        if let Some(tolerance) = &self.tolerance {
            case["tolerance"] = json!(tolerance.reason);
            case["accept"] = Value::Array(tolerance.accept.clone());
            if !tolerance.ignore.is_empty() {
                case["ignore"] = json!(tolerance.ignore);
            }
        }
        case
    }
}

/// The access log lines of one case are those of its authorities; any other line goes to `access-log/other`.
pub fn observation(
    cases: &[Case],
    access_log: &[(Option<String>, String)],
    stats: Map<String, Value>,
) -> Value {
    let claimed = |host: &Option<String>| {
        cases
            .iter()
            .any(|case| host.as_ref().is_some_and(|host| case.hosts.contains(host)))
    };
    let lines = |host: &String| {
        access_log
            .iter()
            .filter(|(line_host, _)| line_host.as_ref() == Some(host))
            .collect::<Vec<_>>()
    };
    let mut values = cases
        .iter()
        .map(|case| {
            case.to_value(
                case.hosts
                    .iter()
                    .flat_map(&lines)
                    .map(|(_, line)| json!(line))
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let other = access_log
        .iter()
        .filter(|(host, _)| !claimed(host))
        .map(|(_, line)| json!(line))
        .collect::<Vec<_>>();
    values.push(json!({ "id": "access-log/other", "steps": [], "access_log": other }));
    json!({ "version": 1, "cases": values, "stats": stats })
}

/// Rewrites every area file, or, with a list of case ids (`stats` for the counters), only those cases.
pub fn record(observed: &Value, selected: Option<&str>) {
    match selected {
        None => fixture::store(observed),
        Some(ids) => fixture::store_selected(observed, &ids.split(',').collect::<Vec<_>>()),
    }
}

pub fn replay(observed: &Value) {
    let problems = compare(&fixture::load(), observed);
    assert!(
        problems.is_empty(),
        "the reverse HTTP/3 path differs from the golden fixture:\n{}",
        problems.join("\n")
    );
}

fn cases_by_id(document: &Value) -> BTreeMap<&str, &Value> {
    let cases = document["cases"].as_array().into_iter().flatten();
    cases
        .map(|case| (case["id"].as_str().expect("case id"), case))
        .collect()
}

fn compare(expected: &Value, observed: &Value) -> Vec<String> {
    let (expected_cases, observed_cases) = (cases_by_id(expected), cases_by_id(observed));
    let mut problems = Vec::new();
    for case in expected["cases"].as_array().into_iter().flatten() {
        let id = case["id"].as_str().expect("case id");
        match observed_cases.get(id) {
            None => problems.push(format!("case {id}: missing from the observed run")),
            Some(observed) => {
                let lines = case_diff(case, observed);
                if !lines.is_empty() {
                    problems.push(format!("case {id}:\n{}", lines.join("\n")));
                }
            }
        }
    }
    for id in observed_cases
        .keys()
        .filter(|id| !expected_cases.contains_key(*id))
    {
        problems.push(format!("case {id}: not in the fixture"));
    }
    let stats = diff("", &expected["stats"], &observed["stats"]);
    if !stats.is_empty() {
        problems.push(format!("stats:\n{}", stats.join("\n")));
    }
    problems
}

fn case_diff(expected: &Value, observed: &Value) -> Vec<String> {
    let mut lines = steps_diff(expected, observed);
    if expected.get("tolerance").is_none() {
        lines.extend(diff(
            "access_log",
            &expected["access_log"],
            &observed["access_log"],
        ));
    }
    lines
}

fn steps_diff(expected: &Value, observed: &Value) -> Vec<String> {
    let (recorded, seen) = (steps(expected), steps(observed));
    let accepted = || expected["accept"].as_array().into_iter().flatten();
    if accepted().any(|entry| entry["steps"].as_array().is_some_and(|steps| steps == seen)) {
        return Vec::new();
    }
    let mut lines = Vec::new();
    if recorded.len() != seen.len() {
        lines.push(format!(
            "  step count: expected {}, observed {}",
            recorded.len(),
            seen.len()
        ));
    }
    let ignored = |path: &str| {
        let prefixes = expected["ignore"].as_array().into_iter().flatten();
        prefixes
            .filter_map(Value::as_str)
            .any(|prefix| path.starts_with(prefix))
    };
    for (index, (step, seen)) in recorded.iter().zip(seen).enumerate() {
        let path = format!("steps[{index}]");
        let unexplained = |candidate: &Value| {
            diff(&path, candidate, seen)
                .into_iter()
                .filter(|line| !ignored(line.trim_start()))
                .collect::<Vec<_>>()
        };
        let differences = unexplained(step);
        if differences.is_empty()
            || alternatives(accepted(), index, step)
                .iter()
                .any(|candidate| unexplained(candidate).is_empty())
        {
            continue;
        }
        lines.extend(differences);
        if let Some(reason) = expected["tolerance"].as_str() {
            lines.push(format!(
                "  (tolerated: {reason}; no accepted alternative matches)"
            ));
        }
    }
    lines
}

fn steps(case: &Value) -> &[Value] {
    case["steps"].as_array().map_or(&[], Vec::as_slice)
}

fn alternatives<'a>(
    accepted: impl Iterator<Item = &'a Value>,
    index: usize,
    recorded: &Value,
) -> Vec<Value> {
    accepted
        .filter(|entry| entry["step"].as_u64() == Some(index as u64))
        .map(|entry| {
            let mut step = recorded.clone();
            for (key, value) in entry
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(key, _)| *key != "step")
            {
                match value {
                    Value::Null => drop(step.as_object_mut().expect("step object").remove(key)),
                    value => step[key] = value.clone(),
                }
            }
            step
        })
        .collect()
}

fn flatten(path: &str, value: &Value, out: &mut BTreeMap<String, String>) {
    match value {
        Value::Object(map) => map
            .iter()
            .for_each(|(key, value)| flatten(&format!("{path}.{key}"), value, out)),
        Value::Array(items) => {
            items
                .iter()
                .enumerate()
                .for_each(|(index, value)| flatten(&format!("{path}[{index}]"), value, out));
        }
        scalar => {
            out.insert(path.to_owned(), scalar.to_string());
        }
    }
}

fn diff(prefix: &str, expected: &Value, observed: &Value) -> Vec<String> {
    let (mut left, mut right) = (BTreeMap::new(), BTreeMap::new());
    flatten(prefix, expected, &mut left);
    flatten(prefix, observed, &mut right);
    let paths = left.keys().chain(right.keys()).collect::<BTreeSet<_>>();
    let show = |side: &BTreeMap<String, String>, path: &String| {
        side.get(path).cloned().unwrap_or_else(|| "<absent>".into())
    };
    paths
        .into_iter()
        .filter(|path| left.get(*path) != right.get(*path))
        .map(|path| {
            format!(
                "  {path}: expected {}, observed {}",
                show(&left, path),
                show(&right, path)
            )
        })
        .collect()
}

/// JSON with one line for every value that fits in `COMPACT_LINE` columns.
pub fn pretty(value: &Value, indent: usize) -> String {
    let compact = value.to_string();
    if compact.len() + indent <= COMPACT_LINE
        || !matches!(value, Value::Object(_) | Value::Array(_))
    {
        return compact;
    }
    let pad = " ".repeat(indent + 2);
    let close = " ".repeat(indent);
    match value {
        Value::Object(map) => {
            let items = map
                .iter()
                .map(|(key, value)| format!("{pad}{}: {}", json!(key), pretty(value, indent + 2)));
            format!("{{\n{}\n{close}}}", items.collect::<Vec<_>>().join(",\n"))
        }
        Value::Array(items) => {
            let items = items
                .iter()
                .map(|value| format!("{pad}{}", pretty(value, indent + 2)));
            format!("[\n{}\n{close}]", items.collect::<Vec<_>>().join(",\n"))
        }
        _ => compact,
    }
}
