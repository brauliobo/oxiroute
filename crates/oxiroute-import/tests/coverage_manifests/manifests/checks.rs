#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
pub(super) fn directive_manifests_are_versioned_and_cover_registered_forms() {
    let nginx: DirectiveManifest<DirectiveForm> = read_manifest("nginx-directives.json");
    let haproxy: DirectiveManifest<DirectiveForm> = read_manifest("haproxy-directives.json");
    let apache: DirectiveManifest<DirectiveForm> = read_manifest("apache-directives.json");
    let squid: SquidDirectiveManifest = read_manifest("squid-directives.json");

    validate_reference(
        &nginx,
        "nginx",
        "git_checkout",
        "eaac3d7",
        "docs/UPSTREAM_ANALYSIS.md",
    );
    validate_reference(
        &haproxy,
        "haproxy",
        "git_checkout",
        "ca686e3",
        "docs/UPSTREAM_ANALYSIS.md",
    );
    validate_reference(
        &apache,
        "apache",
        "git_checkout",
        "2.4.62",
        "docs/UPSTREAM_ANALYSIS.md",
    );
    validate_directive_forms(&nginx.entries);
    validate_directive_forms(&haproxy.entries);
    validate_directive_forms(&apache.entries);
    assert_eq!(squid.schema_version, 2);
    assert_eq!(squid.product, "squid");
    assert_eq!(squid.reference.kind, "git_checkout");
    assert_eq!(squid.reference.value, "6f4c814");
    assert_eq!(squid.reference.documentation, "docs/UPSTREAM_ANALYSIS.md");
    assert!(workspace_path(&squid.reference.documentation).is_file());
    assert_eq!(squid.registry_version, 3);
    assert_eq!(squid.target_version, "6f4c814");
    assert_eq!(squid.profile.id, "squid-forward-http1");
    assert_eq!(squid.profile.version, 3);
    assert_eq!(squid.parity, CapabilityStatus::Partial);
    assert!(!squid.complete_parity);
    assert!(!squid.audit.complete_parity_claim);
    assert!(!squid.families.is_empty());
    assert_unique_ids(
        "Squid family ID",
        squid.families.iter().map(|family| family.id.as_str()),
    );
    validate_squid_families(&squid.families);
    validate_squid_directive_forms(&squid.families, &squid.entries);
}

#[test]
pub(super) fn nginx_rtmp_manifest_matches_all_registry_metadata() {
    let manifest: RtmpDirectiveManifest = read_manifest("nginx-rtmp-directives.json");
    validate_rtmp_reference(&manifest);

    let entries = manifest
        .entries
        .iter()
        .map(|entry| (entry.key.as_str(), entry))
        .collect::<HashMap<_, _>>();
    assert_eq!(
        entries.len(),
        manifest.entries.len(),
        "duplicate nginx-RTMP keys"
    );
    assert_eq!(entries.len(), directive_specs().len());

    for spec in directive_specs() {
        let entry = entries
            .get(spec.name)
            .unwrap_or_else(|| panic!("missing nginx-RTMP registry key `{}`", spec.name));
        assert_rtmp_metadata(entry, spec);
        let arguments = valid_rtmp_arguments(spec);
        for context in spec.contexts {
            assert!(
                validate_directive(spec.name, *context, &arguments).is_ok(),
                "registered nginx-RTMP directive `{}` rejected its authoritative valid probe in {context:?}",
                spec.name
            );
        }
        if let Some(context) = all_rtmp_contexts()
            .into_iter()
            .find(|context| !spec.contexts.contains(context))
        {
            assert!(matches!(
                validate_directive(spec.name, context, &arguments),
                Err(DirectiveError::InvalidContext { .. })
            ));
        }
    }
    for entry in &manifest.entries {
        validate_rtmp_terminal_decision(entry);
        validate_test_categories(&entry.id, &entry.required_tests);
    }
    validate_rtmp_import_forms(&manifest);
}

#[test]
pub(super) fn all_manifest_ids_are_globally_unique() {
    let canonical: CanonicalManifest = read_manifest("canonical.json");
    let nginx: DirectiveManifest<DirectiveForm> = read_manifest("nginx-directives.json");
    let haproxy: DirectiveManifest<DirectiveForm> = read_manifest("haproxy-directives.json");
    let apache: DirectiveManifest<DirectiveForm> = read_manifest("apache-directives.json");
    let rtmp: RtmpDirectiveManifest = read_manifest("nginx-rtmp-directives.json");
    let squid: SquidDirectiveManifest = read_manifest("squid-directives.json");
    let components: ComponentManifest = read_manifest("components.json");

    assert_unique_ids(
        "coverage ID",
        canonical
            .entries
            .iter()
            .map(|entry| entry.id.as_str())
            .chain(nginx.entries.iter().map(|entry| entry.id.as_str()))
            .chain(haproxy.entries.iter().map(|entry| entry.id.as_str()))
            .chain(apache.entries.iter().map(|entry| entry.id.as_str()))
            .chain(rtmp.entries.iter().map(|entry| entry.id.as_str()))
            .chain(rtmp.import_forms.iter().map(|entry| entry.id.as_str()))
            .chain(squid.entries.iter().map(|entry| entry.id.as_str()))
            .chain(components.entries.iter().map(|entry| entry.id.as_str())),
    );
}

#[test]
pub(super) fn manifest_evidence_resolves_to_current_test_functions() {
    let canonical: CanonicalManifest = read_manifest("canonical.json");
    let nginx: DirectiveManifest<DirectiveForm> = read_manifest("nginx-directives.json");
    let haproxy: DirectiveManifest<DirectiveForm> = read_manifest("haproxy-directives.json");
    let apache: DirectiveManifest<DirectiveForm> = read_manifest("apache-directives.json");
    let rtmp: RtmpDirectiveManifest = read_manifest("nginx-rtmp-directives.json");
    let squid: SquidDirectiveManifest = read_manifest("squid-directives.json");
    let components: ComponentManifest = read_manifest("components.json");
    let evidence: EvidenceManifest = read_manifest("evidence.json");
    assert_eq!(evidence.schema_version, 1);

    let manifest_references = [
        canonical.evidence,
        nginx.evidence,
        haproxy.evidence,
        apache.evidence,
        rtmp.evidence,
        squid.evidence,
        components.evidence,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let unique_ids = manifest_references.iter().cloned().collect::<BTreeSet<_>>();
    assert_eq!(
        unique_ids.len(),
        manifest_references.len(),
        "duplicate manifest evidence reference"
    );
    let registered = evidence
        .entries
        .iter()
        .map(|entry| entry.id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        registered.len(),
        evidence.entries.len(),
        "duplicate evidence ID"
    );
    assert_set_equality("manifest evidence IDs", &registered, &unique_ids);

    let mut test_references = HashSet::new();
    for entry in &evidence.entries {
        assert_nonempty_unique(&entry.tests, &entry.id, "test evidence references");
        for reference in &entry.tests {
            assert!(
                test_references.insert(reference),
                "duplicate test evidence reference `{reference}`"
            );
            assert_test_reference(entry, reference);
        }
    }
}
