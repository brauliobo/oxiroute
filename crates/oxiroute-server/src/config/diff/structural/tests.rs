use serde_json::{Value, json};

use super::{ChangeKind, MAX_CHANGES, changes};

fn pointers(old: &Value, new: &Value) -> Vec<(String, ChangeKind)> {
    changes(old, new)
        .0
        .into_iter()
        .map(|change| (change.path, change.kind))
        .collect()
}

#[test]
fn equal_documents_have_no_changes() {
    let document = json!({ "version": 1, "pools": [{ "name": "web" }] });
    assert!(changes(&document, &document).0.is_empty());
}

#[test]
fn named_list_items_are_addressed_by_name() {
    let old = json!({ "upstream_pools": [
        { "name": "web", "servers": [{ "name": "origin-1", "weight": 1 }, { "name": "origin-2", "weight": 1 }] }
    ]});
    let new = json!({ "upstream_pools": [
        { "name": "web", "servers": [{ "name": "origin-1", "weight": 3 }, { "name": "origin-3", "weight": 1 }] }
    ]});
    let found = changes(&old, &new).0;
    let rows = found
        .iter()
        .map(|change| (change.path.as_str(), change.kind))
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            (
                "/upstream_pools/web/servers/origin-1/weight",
                ChangeKind::Changed
            ),
            ("/upstream_pools/web/servers/origin-2", ChangeKind::Removed),
            ("/upstream_pools/web/servers/origin-3", ChangeKind::Added),
        ]
    );
    assert_eq!(found[0].old, Some(json!(1)));
    assert_eq!(found[0].new, Some(json!(3)));
}

#[test]
fn reordering_named_items_is_reported_once() {
    let old = json!({ "routes": [{ "name": "a" }, { "name": "b" }] });
    let new = json!({ "routes": [{ "name": "b" }, { "name": "a" }] });
    let found = changes(&old, &new).0;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, "/routes");
    assert_eq!(found[0].kind, ChangeKind::Reordered);
    assert_eq!(found[0].new, Some(json!(["b", "a"])));
}

#[test]
fn scalar_lists_change_as_a_whole_and_unnamed_objects_by_index() {
    let scalars = pointers(&json!({ "t": ["a", "b"] }), &json!({ "t": ["a"] }));
    assert_eq!(scalars, [("/t".to_owned(), ChangeKind::Changed)]);
    let unnamed = pointers(
        &json!({ "l": [{ "x": 1 }, { "x": 2 }] }),
        &json!({ "l": [{ "x": 1 }, { "x": 3 }, { "x": 4 }] }),
    );
    assert_eq!(
        unnamed,
        [
            ("/l/1/x".to_owned(), ChangeKind::Changed),
            ("/l/2".to_owned(), ChangeKind::Added),
        ]
    );
}

#[test]
fn keys_and_names_escape_slashes_and_tildes() {
    let found = pointers(
        &json!({ "a/b": { "c~d": 1 } }),
        &json!({ "a/b": { "c~d": 2 } }),
    );
    assert_eq!(found, [("/a~1b/c~0d".to_owned(), ChangeKind::Changed)]);
}

#[test]
fn a_type_change_is_one_changed_entry_and_null_is_a_value() {
    let found = pointers(&json!({ "x": null }), &json!({ "x": { "y": 1 } }));
    assert_eq!(found, [("/x".to_owned(), ChangeKind::Changed)]);
}

#[test]
fn the_number_of_changes_is_bounded() {
    let old = Value::Array((0..MAX_CHANGES + 5).map(|_| json!(0)).collect());
    let new = Value::Array((0..MAX_CHANGES + 5).map(|_| json!({ "n": 1 })).collect());
    let (found, truncated) = changes(&old, &new);
    assert_eq!(found.len(), MAX_CHANGES);
    assert!(truncated);
}
