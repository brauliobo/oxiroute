use std::{
    collections::{BTreeSet, HashSet},
    fs,
};

use syn::{Attribute, Fields, GenericArgument, Item, PathArguments, Type};

use crate::{
    manifests::{
        CanonicalManifest, ComponentManifest, Editability, EntryKind, validate_runtime_decision,
        validate_test_categories,
    },
    support::{
        assert_nonempty_unique, assert_set_equality, read_manifest, read_source, workspace_path,
    },
};

#[test]
fn canonical_manifest_is_complete_and_executable() {
    let manifest: CanonicalManifest = read_manifest("canonical.json");

    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.source.crate_name, "oxiroute-config");
    assert_eq!(manifest.source.root_type, "ConfigDraft");
    assert_eq!(manifest.source.config_version, 1);

    let expected = canonical_schema_entries();
    let actual = manifest
        .entries
        .iter()
        .map(|entry| (entry.kind, entry.path.clone()))
        .collect::<BTreeSet<_>>();
    assert_set_equality("canonical schema entries", &expected, &actual);
    assert_eq!(
        actual.len(),
        manifest.entries.len(),
        "duplicate canonical path"
    );

    for entry in &manifest.entries {
        assert!(matches!(
            (entry.kind, entry.editability),
            (EntryKind::Field, Editability::Fixed | Editability::Operator)
                | (EntryKind::Enum, Editability::Fixed)
                | (EntryKind::Variant, Editability::Operator)
        ));
        assert_nonempty_unique(&entry.normalization, &entry.id, "normalization rules");
        assert_nonempty_unique(&entry.validation, &entry.id, "validation rules");
        validate_runtime_decision(&entry.id, &entry.runtime);
        validate_test_categories(&entry.id, &entry.required_tests);
    }
}

#[test]
fn ui_registry_and_controls_cover_the_authoritative_schema() {
    let (mut schema_fields, mut editable_fields) = ui_schema_fields();
    exclude_nonintegrated_component_controls(&mut schema_fields, &mut editable_fields);
    let registry = ui_registry_fields();
    let controls = ui_control_fields();
    let missing_registry = schema_fields.difference(&registry).collect::<Vec<_>>();
    let extra_registry = registry.difference(&schema_fields).collect::<Vec<_>>();
    let unknown_controls = controls.difference(&schema_fields).collect::<Vec<_>>();
    let missing_controls = editable_fields.difference(&controls).collect::<Vec<_>>();
    assert!(
        missing_registry.is_empty()
            && extra_registry.is_empty()
            && unknown_controls.is_empty()
            && missing_controls.is_empty(),
        "UI schema coverage mismatch\nmissing registry fields: {missing_registry:#?}\nextra registry fields: {extra_registry:#?}\nmissing editable controls: {missing_controls:#?}\nunknown controls: {unknown_controls:#?}"
    );
}

fn exclude_nonintegrated_component_controls(
    schema_fields: &mut BTreeSet<String>,
    editable_fields: &mut BTreeSet<String>,
) {
    let components: ComponentManifest = read_manifest("components.json");
    for id in [
        "component.cache-core",
        "component.forward-proxy-h2",
        "component.forward-proxy-h3",
    ] {
        let component = components
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .unwrap_or_else(|| panic!("missing non-integrated component gate `{id}`"));
        assert!(!component.gates.canonical.0, "{id} canonical gate");
    }

    for prefix in [
        "cache_stores",
        "http_services[].routes[].action.policy.cache",
        "max_connections",
        "listeners[].bind.mode",
        "listeners[].downstream_timeouts",
        "upstream_pools[].endpoints",
        "upstream_pools[].servers[].max_connections",
        "upstream_pools[].servers[].dns_resolution",
        "upstream_pools[].queue_timeout_ms",
        "upstream_pools[].connect_timeout_ms",
        "upstream_pools[].server_timeout_ms",
        "upstream_pools[].connection_reuse",
        "upstream_pools[].health_check.startup",
        "upstream_pools[].health_check.fast_interval_ms",
        "upstream_pools[].health_check.down_interval_ms",
        "upstream_pools[].health_check.expected_status",
        "upstream_pools[].health_check.http_version",
        "http_services[].routes[].policy",
        "http_services[].routes[].access_policy.htpasswd_file_path",
        "http_services[].routes[].action.policy.request_headers[].value.name",
        "http_services[].routes[].action.policy.request_headers[].value.max_bytes",
        "http_services[].routes[].action.policy.response_cookie_attributes",
        "http_services[].routes[].action.path_mapping",
        "http_services[].routes[].action.try_files",
        "http_services[].routes[].action.autoindex",
        "http_services[].routes[].action.autoindex_exact_size",
        "http_services[].routes[].action.autoindex_local_time",
        "http_services[].routes[].action.mime",
        "http_services[].routes[].action.error_responses",
        "http_services[].gzip",
        "http_services[].access_log",
        "rtmp_services[].outbound_chunk_size",
        "rtmp_services[].max_inbound_message_size",
        "rtmp_services[].ack_window_size",
        "rtmp_services[].access_log",
        "rtmp_services[].applications[].push_targets",
        "rtmp_services[].applications[].fanout",
        "rtmp_services[].applications[].recorders[].timezone",
        "rtmp_services[].applications[].recorders[].time_basis",
        "rtmp_services[].applications[].recorders[].segment_naming",
    ] {
        assert!(
            editable_fields
                .iter()
                .any(|path| path_has_prefix(path, prefix)),
            "non-integrated UI prefix `{prefix}` has no editable schema fields"
        );
        editable_fields.retain(|path| !path_has_prefix(path, prefix));
    }
    for prefix in [
        "rtmp_services[].max_inbound_message_size",
        "rtmp_services[].ack_window_size",
    ] {
        schema_fields.retain(|path| !path_has_prefix(path, prefix));
        editable_fields.retain(|path| !path_has_prefix(path, prefix));
    }
}

fn path_has_prefix(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with("[]"))
}

fn canonical_schema_entries() -> BTreeSet<(EntryKind, String)> {
    let schema = config_source_schema();
    let mut entries = BTreeSet::new();
    collect_canonical_type(&schema, "ConfigDraft", "", &mut entries);
    entries
}

fn ui_schema_fields() -> (BTreeSet<String>, BTreeSet<String>) {
    let schema = config_source_schema();
    let mut fields = BTreeSet::new();
    let mut editable = BTreeSet::new();
    collect_ui_type(&schema, "ConfigDraft", "", &mut fields, &mut editable);
    (fields, editable)
}

#[test]
fn forward_proxy_cache_uses_the_serialized_wire_path() {
    let canonical = canonical_schema_entries();
    let (ui_fields, editable_fields) = ui_schema_fields();
    let controls = ui_control_fields();

    for path in [
        "forward_proxy_services[].cache",
        "forward_proxy_services[].cache.store",
    ] {
        assert!(
            canonical.contains(&(EntryKind::Field, path.to_owned())),
            "canonical schema omits wire field `{path}`"
        );
        assert!(
            ui_fields.contains(path),
            "UI schema omits wire field `{path}`"
        );
    }
    assert!(
        editable_fields.contains("forward_proxy_services[].cache.store"),
        "wire cache store must remain editable"
    );
    assert!(
        controls.contains("forward_proxy_services[].cache.store"),
        "reusable cache editor omits the wire cache store control"
    );
    assert!(
        !canonical
            .iter()
            .any(|(_, path)| path.starts_with("forward_proxy_services[].header_policy.cache")),
        "canonical schema exposes the skipped in-memory cache path"
    );
    assert!(
        !ui_fields
            .iter()
            .any(|path| path.starts_with("forward_proxy_services[].header_policy.cache")),
        "UI schema exposes the skipped in-memory cache path"
    );
}

#[test]
fn reusable_rtmp_callback_controls_expand_dynamic_call_site_prefixes() {
    let callback_editor = read_source("ui/src/configuration/RtmpCallbackEditor.vue");
    let suffixes = callback_editor
        .lines()
        .flat_map(extract_dynamic_field_suffixes)
        .collect::<BTreeSet<_>>();
    assert_eq!(suffixes.len(), 13, "RTMP callback control suffixes");

    let base_paths = [
        "ui/src/configuration/RtmpServiceEditor.vue",
        "ui/src/configuration/RtmpServicePolicyEditor.vue",
    ]
    .into_iter()
    .flat_map(|path| component_field_paths(&read_source(path), "RtmpCallbackEditor"))
    .collect::<BTreeSet<_>>();
    assert_eq!(
        base_paths,
        BTreeSet::from([
            "rtmp_services[].applications[].callbacks".to_owned(),
            "rtmp_services[].callbacks".to_owned(),
        ]),
        "RTMP callback component call-site prefixes"
    );

    let controls = ui_control_fields();
    for base in base_paths {
        for suffix in &suffixes {
            let path = format!("{base}{suffix}");
            assert!(
                controls.contains(&path),
                "missing expanded control `{path}`"
            );
        }
    }
}

fn collect_ui_type(
    schema: &syn::File,
    type_name: &str,
    prefix: &str,
    fields: &mut BTreeSet<String>,
    editable: &mut BTreeSet<String>,
) {
    let type_name = serialized_schema_type(schema, type_name);
    if let Some(item) = schema.items.iter().find_map(|item| match item {
        Item::Struct(item) if item.ident == type_name => Some(item),
        _ => None,
    }) {
        collect_ui_fields(schema, &item.fields, prefix, fields, editable);
        return;
    }

    let item = schema
        .items
        .iter()
        .find_map(|item| match item {
            Item::Enum(item) if item.ident == type_name => Some(item),
            _ => None,
        })
        .unwrap_or_else(|| panic!("UI schema references unknown type `{type_name}`"));
    if let Some(tag) = serde_enum_tag(&item.attrs) {
        let tag_path = join_path(prefix, &tag);
        fields.insert(tag_path.clone());
        editable.insert(tag_path);
        for variant in &item.variants {
            collect_ui_fields(schema, &variant.fields, prefix, fields, editable);
        }
    } else {
        editable.insert(prefix.to_owned());
    }
}

fn collect_ui_fields(
    schema: &syn::File,
    source_fields: &Fields,
    prefix: &str,
    fields: &mut BTreeSet<String>,
    editable: &mut BTreeSet<String>,
) {
    let Fields::Named(source_fields) = source_fields else {
        return;
    };
    for field in &source_fields.named {
        if serde_field_skipped(&field.attrs) {
            continue;
        }
        let (inner, collection) = unwrap_schema_type(&field.ty);
        if serde_field_flattened(&field.attrs) {
            let type_name = rust_type_name(inner).expect("flattened schema field type");
            collect_ui_type(schema, &type_name, prefix, fields, editable);
            continue;
        }
        let name = serialized_field_name(field.ident.as_ref().expect("named field"), &field.attrs);
        let path = join_path(prefix, &name);
        fields.insert(path.clone());
        let Some(type_name) = rust_type_name(inner) else {
            editable.insert(path);
            continue;
        };
        if schema_type_exists(schema, &type_name) {
            let child_prefix = if collection && schema_object_type(schema, &type_name) {
                format!("{path}[]")
            } else {
                path
            };
            collect_ui_type(schema, &type_name, &child_prefix, fields, editable);
        } else {
            editable.insert(path);
        }
    }
}

fn serde_enum_tag(attributes: &[Attribute]) -> Option<String> {
    let mut tag = None;
    for attribute in attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("serde"))
    {
        attribute
            .parse_nested_meta(|meta| {
                if meta.path.is_ident("tag") {
                    tag = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                } else if meta.input.peek(syn::Token![=]) {
                    let _: syn::Expr = meta.value()?.parse()?;
                }
                Ok(())
            })
            .expect("valid serde enum attribute");
    }
    tag
}

fn serde_field_flattened(attributes: &[Attribute]) -> bool {
    let mut flattened = false;
    for attribute in attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("serde"))
    {
        attribute
            .parse_nested_meta(|meta| {
                if meta.path.is_ident("flatten") {
                    flattened = true;
                } else if meta.input.peek(syn::Token![=]) {
                    let _: syn::Expr = meta.value()?.parse()?;
                }
                Ok(())
            })
            .expect("valid serde field attribute");
    }
    flattened
}

fn serde_field_skipped(attributes: &[Attribute]) -> bool {
    let mut skipped = false;
    for attribute in attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("serde"))
    {
        attribute
            .parse_nested_meta(|meta| {
                if meta.path.is_ident("skip") {
                    skipped = true;
                } else if meta.input.peek(syn::Token![=]) {
                    let _: syn::Expr = meta.value()?.parse()?;
                }
                Ok(())
            })
            .expect("valid serde field attribute");
    }
    skipped
}

fn ui_registry_fields() -> BTreeSet<String> {
    let source = read_source("ui/src/config/fieldRegistry.ts");
    let registry = source
        .split("export const CANONICAL_FIELD_REGISTRY = [")
        .nth(1)
        .and_then(|source| source.split("] as const").next())
        .expect("locate CANONICAL_FIELD_REGISTRY");
    let paths = registry
        .lines()
        .filter_map(|line| extract_quoted_attribute(line, "{ path: '"))
        .collect::<Vec<_>>();
    assert_eq!(
        paths.iter().collect::<HashSet<_>>().len(),
        paths.len(),
        "UI registry repeats canonical fields"
    );
    paths.into_iter().collect()
}

fn ui_control_fields() -> BTreeSet<String> {
    let mut paths = vec![workspace_path("ui/src/ConfigurationWorkspace.vue")];
    let mut editors = fs::read_dir(workspace_path("ui/src/configuration"))
        .expect("read modular configuration editors")
        .map(|entry| entry.expect("read configuration editor entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("vue"))
        })
        .collect::<Vec<_>>();
    editors.sort();
    paths.extend(editors);
    let sources = paths
        .into_iter()
        .map(|path| {
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            (path, source)
        })
        .collect::<Vec<_>>();
    let mut controls = sources
        .iter()
        .flat_map(|(_, source)| {
            source
                .lines()
                .filter(|line| !line.contains(":data-field"))
                .flat_map(|line| {
                    [
                        extract_quoted_attribute(line, "data-field=\""),
                        extract_quoted_attribute(line, "field-path=\""),
                    ]
                    .into_iter()
                    .flatten()
                })
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>();

    for component in ["HttpCachePolicyEditor", "RtmpCallbackEditor"] {
        let editor_name = format!("{component}.vue");
        let (_, editor) = sources
            .iter()
            .find(|(path, _)| path.ends_with(&editor_name))
            .unwrap_or_else(|| panic!("locate reusable {component}"));
        let dynamic_suffixes = editor
            .lines()
            .flat_map(extract_dynamic_field_suffixes)
            .collect::<BTreeSet<_>>();
        let base_paths = sources
            .iter()
            .flat_map(|(_, source)| component_field_paths(source, component))
            .collect::<BTreeSet<_>>();
        for base in base_paths {
            controls.extend(
                dynamic_suffixes
                    .iter()
                    .map(|suffix| format!("{base}{suffix}")),
            );
        }
    }
    controls
}

fn component_field_paths(source: &str, component: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut in_component = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(&format!("{component}(")) {
            in_component = true;
        }
        if in_component {
            if let Some(path) = extract_quoted_attribute(line, "field-path=\"") {
                paths.push(path);
            }
            if trimmed == ")" {
                in_component = false;
            }
        }
    }
    paths
}

fn extract_dynamic_field_suffixes(line: &str) -> Vec<String> {
    line.split("`${fieldPath}")
        .skip(1)
        .filter_map(|value| value.split('`').next())
        .filter(|suffix| !suffix.is_empty())
        .map(str::to_owned)
        .collect()
}

fn extract_quoted_attribute(line: &str, prefix: &str) -> Option<String> {
    let value = line.split_once(prefix)?.1;
    let delimiter = if prefix.ends_with('\'') { '\'' } else { '"' };
    value.split(delimiter).next().map(str::to_owned)
}

fn collect_canonical_type(
    schema: &syn::File,
    type_name: &str,
    prefix: &str,
    entries: &mut BTreeSet<(EntryKind, String)>,
) {
    let type_name = serialized_schema_type(schema, type_name);
    if let Some(item) = schema.items.iter().find_map(|item| match item {
        Item::Struct(item) if item.ident == type_name => Some(item),
        _ => None,
    }) {
        collect_canonical_fields(schema, &item.fields, prefix, entries);
        return;
    }

    let item = schema
        .items
        .iter()
        .find_map(|item| match item {
            Item::Enum(item) if item.ident == type_name => Some(item),
            _ => None,
        })
        .unwrap_or_else(|| panic!("canonical schema references unknown type `{type_name}`"));
    entries.insert((EntryKind::Enum, type_name.clone()));
    for variant in &item.variants {
        entries.insert((
            EntryKind::Variant,
            format!("{type_name}::{}", variant.ident),
        ));
        if matches!(variant.fields, Fields::Named(_)) {
            let variant_prefix = format!("{prefix}<{}>", snake_case(&variant.ident.to_string()));
            collect_canonical_fields(schema, &variant.fields, &variant_prefix, entries);
        }
    }
}

fn collect_canonical_fields(
    schema: &syn::File,
    fields: &Fields,
    prefix: &str,
    entries: &mut BTreeSet<(EntryKind, String)>,
) {
    let Fields::Named(fields) = fields else {
        return;
    };
    for field in &fields.named {
        if serde_field_skipped(&field.attrs) {
            continue;
        }
        let (inner, collection) = unwrap_schema_type(&field.ty);
        if serde_field_flattened(&field.attrs) {
            let type_name = rust_type_name(inner).expect("flattened canonical field type");
            let item = schema.items.iter().find_map(|item| match item {
                Item::Enum(item) if item.ident == type_name => Some(item),
                _ => None,
            });
            if let Some(tag) = item.and_then(|item| serde_enum_tag(&item.attrs)) {
                entries.insert((EntryKind::Field, join_path(prefix, &tag)));
            }
            collect_canonical_type(schema, &type_name, prefix, entries);
            continue;
        }
        let name = serialized_field_name(field.ident.as_ref().expect("named field"), &field.attrs);
        let base = join_path(prefix, &name);
        let path = if collection {
            format!("{base}[]")
        } else {
            base
        };
        assert!(
            entries.insert((EntryKind::Field, path.clone())),
            "duplicate canonical schema field `{path}`"
        );
        if let Some(type_name) =
            rust_type_name(inner).filter(|name| schema_type_exists(schema, name))
        {
            collect_canonical_type(schema, &type_name, &path, entries);
        }
    }
}

fn config_source_schema() -> syn::File {
    let mut schema = syn::parse_file("").expect("parse empty configuration schema");
    for path in [
        "crates/oxiroute-config/src/model/core.rs",
        "crates/oxiroute-config/src/model/http.rs",
        "crates/oxiroute-config/src/model/rtmp.rs",
        "crates/oxiroute-config/src/model/forward.rs",
        "crates/oxiroute-config/src/model/errors.rs",
    ] {
        let source = read_source(path);
        let parsed = syn::parse_file(&source).expect("parse authoritative configuration schema");
        schema.items.extend(parsed.items);
    }
    schema
}

fn serialized_field_name(ident: &syn::Ident, attributes: &[Attribute]) -> String {
    let mut renamed = None;
    for attribute in attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("serde"))
    {
        attribute
            .parse_nested_meta(|meta| {
                if meta.path.is_ident("rename") {
                    renamed = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                } else if meta.input.peek(syn::Token![=]) {
                    let _: syn::Expr = meta.value()?.parse()?;
                }
                Ok(())
            })
            .expect("valid serde field attribute");
    }
    renamed.unwrap_or_else(|| ident.to_string())
}

fn unwrap_schema_type(mut ty: &Type) -> (&Type, bool) {
    let mut collection = false;
    loop {
        let Type::Path(path) = ty else {
            return (ty, collection);
        };
        let Some(segment) = path.path.segments.last() else {
            return (ty, collection);
        };
        let wrapper = segment.ident.to_string();
        if wrapper != "Box" && wrapper != "Option" && wrapper != "Vec" {
            return (ty, collection);
        }
        collection |= wrapper == "Vec";
        let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
            return (ty, collection);
        };
        let Some(GenericArgument::Type(inner)) = arguments.args.first() else {
            return (ty, collection);
        };
        ty = inner;
    }
}

fn rust_type_name(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else {
        return None;
    };
    path.path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
}

fn schema_type_exists(schema: &syn::File, name: &str) -> bool {
    schema.items.iter().any(|item| match item {
        Item::Struct(item) => item.ident == name,
        Item::Enum(item) => item.ident == name,
        _ => false,
    })
}

fn serialized_schema_type(schema: &syn::File, name: &str) -> String {
    let wire_name = format!("{name}Wire");
    if schema_type_exists(schema, &wire_name) {
        wire_name
    } else {
        name.to_owned()
    }
}

fn schema_object_type(schema: &syn::File, name: &str) -> bool {
    schema.items.iter().any(|item| match item {
        Item::Struct(item) => item.ident == name,
        Item::Enum(item) => item.ident == name && serde_enum_tag(&item.attrs).is_some(),
        _ => false,
    })
}

fn join_path(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}.{name}")
    }
}

fn snake_case(value: &str) -> String {
    let mut result = String::new();
    for (index, character) in value.chars().enumerate() {
        if character.is_ascii_uppercase() {
            if index > 0 {
                result.push('_');
            }
            result.push(character.to_ascii_lowercase());
        } else {
            result.push(character);
        }
    }
    result
}
