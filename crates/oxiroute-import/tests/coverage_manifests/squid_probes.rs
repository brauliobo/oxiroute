use std::{collections::BTreeSet, path::Path};

use oxiroute_import::squid::{
    Activation, DecisionOutcome, DirectiveFamily, SemanticBlockerKind, SquidCapabilityStatus,
    import, squid_capability_report,
};

use crate::{
    manifests::{CapabilityStatus, SquidDirectiveManifest},
    support::{assert_set_equality, read_manifest, workspace_path},
};

#[test]
fn squid_hostrouter_manifest_accounts_for_the_exact_sanitized_inventory() {
    assert_hostrouter_squid_inventory();
}

pub(crate) fn assert_hostrouter_squid_inventory() {
    let manifest: SquidDirectiveManifest = read_manifest("squid-directives.json");
    let fixture = workspace_path(&manifest.audit.fixture);
    assert!(fixture.is_file());
    assert_eq!(manifest.audit.expanded_directives, 35);
    assert_eq!(manifest.audit.access_rules, 9);
    assert!(manifest.audit.canonical_claim);
    assert!(manifest.audit.runtime_claim);
    assert!(!manifest.audit.complete_parity_claim);

    let report = import(Path::new(&fixture));
    assert_eq!(report.source_graph.expanded_directives.len(), 35);
    assert_eq!(report.decision_ledger.decisions.len(), 35);
    assert_eq!(report.effective.access_rules.len(), 9);
    assert!(report.config.is_some());
    assert_eq!(report.draft.listeners.len(), 1);
    assert!(!report.canonical_provenance.is_empty());
    let paths = report
        .canonical_provenance
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(paths.len(), report.canonical_provenance.len());
    assert!(paths.contains("/listeners/0"));
    assert!(paths.contains("/forward_proxy_services/0"));
    assert!(
        report
            .canonical_provenance
            .iter()
            .all(|entry| !entry.origins.is_empty())
    );

    let observed = report
        .decision_ledger
        .decisions
        .iter()
        .map(|decision| String::from_utf8_lossy(&decision.name).into_owned())
        .collect::<BTreeSet<_>>();
    let manifested = manifest
        .entries
        .iter()
        .filter(|entry| observed.contains(&entry.key))
        .map(|entry| entry.key.clone())
        .collect::<BTreeSet<_>>();
    assert_set_equality("hostrouter Squid directive keys", &observed, &manifested);

    for (ordinal, decision) in report.decision_ledger.decisions.iter().enumerate() {
        assert_eq!(decision.origin.occurrence.get(), ordinal);
        let DecisionOutcome::Classified {
            family, activation, ..
        } = decision.outcome;
        assert_ne!(family, DirectiveFamily::Unknown);
        assert!(!matches!(
            activation,
            Activation::Blocked(
                SemanticBlockerKind::InvalidForm | SemanticBlockerKind::UnknownDirective
            )
        ));
    }
}

#[test]
fn squid_registry_matches_target_manifest_and_never_claims_complete_parity() {
    let manifest: SquidDirectiveManifest = read_manifest("squid-directives.json");
    let registry = squid_capability_report();

    assert_eq!(registry.registry_version, manifest.registry_version);
    assert_eq!(registry.product, manifest.product);
    assert_eq!(registry.reference.value, manifest.target_version);
    assert_eq!(registry.target_version, manifest.target_version);
    assert_eq!(registry.profile.id, manifest.profile.id);
    assert_eq!(registry.profile.version, manifest.profile.version);
    assert_eq!(
        registry.parity.as_str(),
        capability_status_name(manifest.parity)
    );
    assert!(!manifest.complete_parity);
    assert!(!registry.complete_parity);
    assert!(registry.has_open_entries());
    assert!(!registry.allows_complete_parity());
    assert!(
        registry
            .families
            .iter()
            .any(|family| family.status == SquidCapabilityStatus::Partial)
    );
    assert!(
        registry
            .families
            .iter()
            .any(|family| family.status == SquidCapabilityStatus::Unsupported)
    );

    assert_eq!(registry.families.len(), manifest.families.len());
    for family in registry.families {
        let manifested = manifest
            .families
            .iter()
            .find(|entry| entry.id == family.id)
            .unwrap_or_else(|| panic!("missing Squid family {}", family.id));
        assert_eq!(
            capability_status_name(manifested.status),
            family.status.as_str()
        );
        assert!(!manifested.rationale.is_empty());
        assert!(!manifested.current_evidence.is_empty());
        assert!(!manifested.required_tests.is_empty());
    }

    assert_eq!(registry.directives.len(), manifest.entries.len());
    for directive in registry.directives {
        let manifested = manifest
            .entries
            .iter()
            .find(|entry| entry.id == directive.id)
            .unwrap_or_else(|| panic!("missing Squid directive {}", directive.id));
        assert_eq!(manifested.key, directive.key);
        assert_eq!(manifested.family, directive.family);
        assert_eq!(
            capability_status_name(manifested.status),
            directive.status.as_str()
        );
        assert!(!manifested.rationale.is_empty());
        assert!(!manifested.current_evidence.is_empty());
        assert!(!manifested.required_tests.is_empty());
        if directive.status == SquidCapabilityStatus::Compatible {
            assert!(directive.required_tests.contains(&"integration"));
            assert!(directive.required_tests.contains(&"failure"));
        }
    }
}

fn capability_status_name(status: CapabilityStatus) -> &'static str {
    match status {
        CapabilityStatus::Compatible => "compatible",
        CapabilityStatus::Partial => "partial",
        CapabilityStatus::Unsupported => "unsupported",
        CapabilityStatus::Obsolete => "obsolete",
        CapabilityStatus::NotPlanned => "not_planned",
    }
}
