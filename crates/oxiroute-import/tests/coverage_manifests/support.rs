use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

pub(crate) fn workspace_root() -> PathBuf {
    fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .expect("canonical workspace root")
}

pub(crate) fn workspace_path(relative_path: impl AsRef<Path>) -> PathBuf {
    workspace_root().join(relative_path)
}

pub(crate) fn read_source(relative_path: impl AsRef<Path>) -> String {
    let path = workspace_path(relative_path);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

pub(crate) fn read_json<T: for<'de> Deserialize<'de>>(relative_path: impl AsRef<Path>) -> T {
    let path = workspace_path(relative_path);
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&source)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

pub(crate) fn read_manifest<T: for<'de> Deserialize<'de>>(name: &str) -> T {
    read_json(Path::new("coverage").join(name))
}

pub(crate) fn assert_unique_ids<'a>(label: &str, ids: impl IntoIterator<Item = &'a str>) {
    let mut unique = HashSet::new();
    for id in ids {
        assert!(unique.insert(id), "duplicate {label} `{id}`");
    }
}

pub(crate) fn assert_nonempty_unique(values: &[String], id: &str, label: &str) {
    assert!(!values.is_empty(), "coverage entry `{id}` has no {label}");
    assert!(
        values.iter().all(|value| !value.is_empty()),
        "coverage entry `{id}` has an empty {label} value"
    );
    assert_eq!(
        values.iter().collect::<HashSet<_>>().len(),
        values.len(),
        "coverage entry `{id}` repeats {label}"
    );
}

pub(crate) fn assert_set_equality<T>(label: &str, expected: &BTreeSet<T>, actual: &BTreeSet<T>)
where
    T: std::fmt::Debug + Ord,
{
    let missing = expected.difference(actual).collect::<Vec<_>>();
    let extra = actual.difference(expected).collect::<Vec<_>>();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{label} mismatch\nmissing: {missing:#?}\nextra: {extra:#?}"
    );
}

pub(crate) fn reference_parts<'a>(id: &str, reference: &'a str) -> (&'a str, &'a str) {
    let (relative_path, function_name) = reference
        .split_once('#')
        .unwrap_or_else(|| panic!("{id} evidence must use path#test_function: {reference}"));
    assert!(
        !function_name.is_empty() && !function_name.contains('#'),
        "{id} has an invalid test function reference: {reference}"
    );
    (relative_path, function_name)
}
