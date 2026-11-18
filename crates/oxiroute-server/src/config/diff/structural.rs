use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::{Map, Value};

pub(crate) const MAX_CHANGES: usize = 500;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChangeKind {
    Added,
    Removed,
    Changed,
    Reordered,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Change {
    pub(crate) path: String,
    pub(crate) kind: ChangeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) old: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) new: Option<Value>,
}

impl Change {
    fn added(path: String, new: &Value) -> Self {
        Self {
            path,
            kind: ChangeKind::Added,
            old: None,
            new: Some(new.clone()),
        }
    }

    fn removed(path: String, old: &Value) -> Self {
        Self {
            path,
            kind: ChangeKind::Removed,
            old: Some(old.clone()),
            new: None,
        }
    }

    fn replaced(path: String, kind: ChangeKind, old: Value, new: Value) -> Self {
        Self {
            path,
            kind,
            old: Some(old),
            new: Some(new),
        }
    }
}

/// Every setting that differs between two configurations, as JSON pointers that address list
/// items by name (`/upstream_pools/web/servers/origin-1/weight`) when every item of both lists
/// has a unique `name`, and by index otherwise. At most [`MAX_CHANGES`] are returned; the flag
/// tells whether more exist.
pub(crate) fn changes(old: &Value, new: &Value) -> (Vec<Change>, bool) {
    let mut found = Vec::new();
    walk(String::new(), old, new, &mut found);
    let truncated = found.len() > MAX_CHANGES;
    found.truncate(MAX_CHANGES);
    (found, truncated)
}

fn walk(path: String, old: &Value, new: &Value, found: &mut Vec<Change>) {
    if old == new {
        return;
    }
    match (old, new) {
        (Value::Object(old), Value::Object(new)) => objects(&path, old, new, found),
        (Value::Array(old), Value::Array(new)) => arrays(path, old, new, found),
        _ => found.push(Change::replaced(
            path,
            ChangeKind::Changed,
            old.clone(),
            new.clone(),
        )),
    }
}

fn arrays(path: String, old: &[Value], new: &[Value], found: &mut Vec<Change>) {
    if let (Some(old_names), Some(new_names)) = (names(old), names(new)) {
        return named_items(&path, (old, &old_names), (new, &new_names), found);
    }
    if old.iter().chain(new).any(is_container) {
        return indexed_items(&path, old, new, found);
    }
    found.push(Change::replaced(
        path,
        ChangeKind::Changed,
        Value::Array(old.to_vec()),
        Value::Array(new.to_vec()),
    ));
}

fn objects(
    path: &str,
    old: &Map<String, Value>,
    new: &Map<String, Value>,
    found: &mut Vec<Change>,
) {
    let keys = old.keys().chain(new.keys()).collect::<BTreeSet<_>>();
    for key in keys {
        entry(child_path(path, key), old.get(key), new.get(key), found);
    }
}

fn entry(path: String, old: Option<&Value>, new: Option<&Value>, found: &mut Vec<Change>) {
    match (old, new) {
        (Some(old), Some(new)) => walk(path, old, new, found),
        (Some(old), None) => found.push(Change::removed(path, old)),
        (None, Some(new)) => found.push(Change::added(path, new)),
        (None, None) => {}
    }
}

fn named_items(
    path: &str,
    (old, old_names): (&[Value], &[&str]),
    (new, new_names): (&[Value], &[&str]),
    found: &mut Vec<Change>,
) {
    let common_old = old_names.iter().filter(|name| new_names.contains(name));
    let common_new = new_names.iter().filter(|name| old_names.contains(name));
    if common_old.ne(common_new) {
        found.push(Change::replaced(
            path.to_owned(),
            ChangeKind::Reordered,
            Value::from(old_names.to_vec()),
            Value::from(new_names.to_vec()),
        ));
    }
    for (name, item) in old_names.iter().zip(old) {
        let counterpart = new_names
            .iter()
            .position(|candidate| candidate == name)
            .map(|index| &new[index]);
        entry(child_path(path, name), Some(item), counterpart, found);
    }
    for (name, item) in new_names.iter().zip(new) {
        if !old_names.contains(name) {
            entry(child_path(path, name), None, Some(item), found);
        }
    }
}

fn indexed_items(path: &str, old: &[Value], new: &[Value], found: &mut Vec<Change>) {
    for index in 0..old.len().max(new.len()) {
        entry(
            child_path(path, &index.to_string()),
            old.get(index),
            new.get(index),
            found,
        );
    }
}

fn names(items: &[Value]) -> Option<Vec<&str>> {
    let names = items
        .iter()
        .map(|item| item.get("name")?.as_str())
        .collect::<Option<Vec<_>>>()?;
    let unique = names.iter().collect::<BTreeSet<_>>().len() == names.len();
    unique.then_some(names)
}

fn is_container(value: &Value) -> bool {
    value.is_object() || value.is_array()
}

fn child_path(parent: &str, segment: &str) -> String {
    format!("{parent}/{}", segment.replace('~', "~0").replace('/', "~1"))
}

#[cfg(test)]
mod tests;
