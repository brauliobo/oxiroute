use oxiroute_import::{
    SourceId,
    haproxy::{
        BlockingReason, DefaultsSelection, E_CONFLICTING_DIRECTIVE, E_DUPLICATE_IDENTITY,
        E_UNRESOLVED_REFERENCE, ProxyMode, parse_sources, resolve_parsed,
    },
};

use super::common::{code_count, loaded, occurrence_count, resolve_bytes};

#[test]
fn defaults_chains_preserve_source_order_and_exact_inheritance_steps() {
    let contents = b"defaults base
  mode tcp
  timeout connect 1s
defaults web from base
  mode http
  timeout server 2s
frontend implicit_web
  bind :80
defaults latest
  mode tcp
frontend implicit_latest
  bind :81
frontend explicit_web from web
  bind :82
";
    let (_, report) = resolve_bytes(contents);

    assert!(report.diagnostics().is_empty());
    assert_eq!(
        report
            .value()
            .defaults
            .iter()
            .map(|defaults| defaults.section.name.as_deref().unwrap())
            .collect::<Vec<_>>(),
        [b"base".as_slice(), b"web".as_slice(), b"latest".as_slice()]
    );
    let web = &report.value().defaults[1];
    let inherited_connect = web.settings.timeouts.connect.as_ref().unwrap();
    assert_eq!(inherited_connect.provenance.inheritance.len(), 1);
    assert_eq!(
        inherited_connect.provenance.inheritance[0].selection,
        DefaultsSelection::Explicit
    );

    let implicit_web = &report.value().frontends[0];
    assert_eq!(
        implicit_web.defaults.as_ref().unwrap().selection,
        DefaultsSelection::ImplicitLatest
    );
    assert_eq!(
        implicit_web.settings.mode.as_ref().unwrap().value,
        ProxyMode::Http
    );
    assert_eq!(
        implicit_web
            .settings
            .timeouts
            .connect
            .as_ref()
            .unwrap()
            .provenance
            .inheritance
            .len(),
        2
    );

    let implicit_latest = &report.value().frontends[1];
    assert_eq!(
        implicit_latest.settings.mode.as_ref().unwrap().value,
        ProxyMode::Tcp
    );
    assert!(implicit_latest.settings.timeouts.connect.is_none());

    let explicit_web = &report.value().frontends[2];
    assert_eq!(
        explicit_web.defaults.as_ref().unwrap().selection,
        DefaultsSelection::Explicit
    );
    assert_eq!(
        explicit_web.settings.mode.as_ref().unwrap().value,
        ProxyMode::Http
    );
    assert_eq!(
        explicit_web
            .settings
            .timeouts
            .connect
            .as_ref()
            .unwrap()
            .provenance
            .inheritance
            .len(),
        2
    );
}

#[test]
fn parsed_source_vector_order_controls_latest_defaults_across_files() {
    let parsed = parse_sources(&[
        loaded(7, "first.cfg", b"defaults first\n  mode tcp\n"),
        loaded(2, "second.cfg", b"defaults second\n  mode http\n"),
        loaded(
            9,
            "third.cfg",
            b"frontend public\n  bind /run/haproxy/public.sock\n",
        ),
    ]);
    let report = resolve_parsed(parsed);

    assert!(report.diagnostics().is_empty());
    assert_eq!(
        report.value().frontends[0]
            .defaults
            .as_ref()
            .unwrap()
            .section
            .source,
        SourceId::new(2)
    );
    assert_eq!(
        report.value().frontends[0]
            .settings
            .mode
            .as_ref()
            .unwrap()
            .value,
        ProxyMode::Http
    );
}

#[test]
fn explicit_defaults_reference_requires_one_unique_target() {
    let contents = b"defaults shared
  mode http
defaults shared
  mode tcp
frontend ambiguous from shared
  bind :80
frontend missing from absent
  bind :81
";
    let (_, report) = resolve_bytes(contents);

    assert_eq!(code_count(&report, E_DUPLICATE_IDENTITY), 1);
    assert_eq!(code_count(&report, E_UNRESOLVED_REFERENCE), 2);
    assert!(
        report
            .value()
            .frontends
            .iter()
            .all(|frontend| frontend.defaults.is_none() && frontend.settings.mode.is_none())
    );
}

#[test]
fn duplicate_identities_are_blocking_and_never_overwrite_occurrences() {
    let contents = b"defaults base
  mode http
frontend public
  bind :80
frontend public
  bind :81
backend app
  server node node1.test:3000
  server node node2.test:3000
backend app
  server other node3.test:3000
";
    let (configuration, report) = resolve_bytes(contents);

    assert_eq!(code_count(&report, E_DUPLICATE_IDENTITY), 3);
    assert_eq!(report.value().frontends.len(), 2);
    assert_eq!(report.value().backends.len(), 2);
    assert_eq!(report.value().backends[0].servers.len(), 2);
    assert_eq!(
        report.value().ledger.len(),
        occurrence_count(&configuration)
    );
    assert!(report.value().ledger.iter().any(|decision| {
        decision.outcome
            == oxiroute_import::haproxy::DecisionOutcome::Blocked(BlockingReason::DuplicateIdentity)
    }));
}

#[test]
fn conflicting_scalar_and_option_directives_are_blocking() {
    let contents = b"defaults shared
  mode http
  mode tcp
  timeout connect 1s
  timeout connect 2s
  option redispatch
  no option redispatch
frontend public
  bind 127.0.0.1:8080
";
    let (_, report) = resolve_bytes(contents);

    assert_eq!(code_count(&report, E_CONFLICTING_DIRECTIVE), 3);
    assert_eq!(
        report.value().defaults[0].settings.semantic_blockers.len(),
        3
    );
    assert!(
        report
            .value()
            .ledger
            .iter()
            .filter(|decision| {
                decision.outcome
                    == oxiroute_import::haproxy::DecisionOutcome::Blocked(
                        BlockingReason::ConflictingDirective,
                    )
            })
            .count()
            >= 3
    );
}
