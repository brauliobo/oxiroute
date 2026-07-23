use std::{collections::BTreeSet, path::Path};

use oxiroute_import::squid::{
    Activation, DecisionOutcome, DirectiveFamily, SemanticBlockerKind, import,
};

use crate::{
    manifests::SquidDirectiveManifest,
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
    assert!(!manifest.audit.canonical_claim);
    assert!(!manifest.audit.runtime_claim);

    let report = import(Path::new(&fixture));
    assert_eq!(report.source_graph.expanded_directives.len(), 35);
    assert_eq!(report.decision_ledger.decisions.len(), 35);
    assert_eq!(report.effective.access_rules.len(), 9);
    assert!(report.config.is_none());
    assert!(report.draft.listeners.is_empty());
    assert!(report.canonical_provenance.is_empty());

    let manifested = manifest
        .entries
        .iter()
        .map(|entry| entry.key.clone())
        .collect::<BTreeSet<_>>();
    let observed = report
        .decision_ledger
        .decisions
        .iter()
        .map(|decision| String::from_utf8_lossy(&decision.name).into_owned())
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
