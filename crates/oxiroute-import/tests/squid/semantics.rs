use std::fs;

use oxiroute_import::{
    SourceFile, SourceId,
    squid::{
        AclType, Activation, AuthenticationValue, DecisionOutcome, DirectiveResolution,
        DirectiveSemantics, LogDestination, PrivacyDirective, SemanticBlockerKind,
        SquidLoweringAdapter, import, lex, parse,
    },
};
use tempfile::tempdir;

use super::common::fixture;

#[test]
fn lexer_preserves_bytes_while_parser_decodes_quotes_comments_and_continuations() {
    let bytes = b"acl local src 192.0.2.0/24 \\\r\n  198.51.100.0/24 # synthetic\r\ninclude \"conf.d/with space.conf\"\n";
    let source = SourceFile::new(SourceId::new(7), "squid.conf", bytes.as_slice());

    let lexed = lex(&source);
    assert!(lexed.diagnostics().is_empty());
    assert_eq!(lexed.value().len(), 2);
    assert_eq!(lexed.value()[0].words.len(), 5);
    assert_eq!(lexed.value()[0].comments.len(), 1);
    assert_eq!(source.bytes(), bytes);

    let parsed = parse(&source);
    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.value().directives.len(), 2);
    assert_eq!(
        parsed.value().directives[1].arguments[0].value,
        b"conf.d/with space.conf"
    );
}

#[test]
fn cache_policy_and_storage_are_classified_without_placeholders() {
    let directory = tempdir().expect("temp directory");
    let root = directory.path().join("squid.conf");
    fs::write(
        &root,
        b"cache_mem 16 MB\ncache_dir ufs /tmp/squid-synthetic-cache 16 16 256\n",
    )
    .expect("root source");

    let report = import(&root);
    assert_eq!(report.effective.cache_policy.len(), 1);
    assert_eq!(report.effective.storage.len(), 1);
    assert_eq!(report.candidate.summary().listeners, 0);
    assert!(
        report
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    for kind in [
        SemanticBlockerKind::ForwardProxyListener,
        SemanticBlockerKind::CachePolicy,
        SemanticBlockerKind::StoragePolicy,
    ] {
        assert!(
            report
                .blocked_capabilities
                .iter()
                .any(|blocked| blocked.kind == kind)
        );
    }
}

#[test]
fn lowering_adapter_receives_typed_ir_without_a_duplicate_schema() {
    struct CountAdapter;

    impl SquidLoweringAdapter for CountAdapter {
        type Output = (usize, usize, usize);
        type Error = std::convert::Infallible;

        fn lower(
            &self,
            source: oxiroute_import::squid::LoweringView<'_>,
        ) -> Result<Self::Output, Self::Error> {
            Ok((
                source.effective.ports.len(),
                source.decision_ledger.decisions.len(),
                source.blocked_capabilities.len(),
            ))
        }
    }

    let report = import(&fixture("forward-proxy-auth.conf"));
    assert_eq!(report.lower_with(&CountAdapter), Ok((1, 35, 0)));
}

#[test]
fn header_privacy_dns_logging_and_port_options_have_typed_semantics() {
    let directory = tempdir().expect("temp directory");
    let root = directory.path().join("squid.conf");
    fs::write(
        &root,
        b"acl clients src 192.0.2.0/24\n\
          http_port 192.0.2.10:3128 intercept name=synthetic\n\
          request_header_access X-Synthetic deny clients\n\
          request_header_replace X-Synthetic synthetic-value\n\
          dns_nameservers 192.0.2.53\n\
          access_log stdio:/tmp/synthetic-access.log squid\n\
          forwarded_for truncate\n\
          via on\n",
    )
    .expect("synthetic source");

    let report = import(&root);
    assert_eq!(report.effective.ports.len(), 1);
    assert_eq!(report.effective.ports[0].options.len(), 2);
    assert_eq!(report.effective.access_policies.len(), 1);
    assert_eq!(
        report.effective.access_policies[0].rules[0]
            .selector
            .as_ref()
            .expect("header selector")
            .value,
        b"X-Synthetic"
    );
    assert!(matches!(
        report.effective.privacy[0],
        PrivacyDirective::HeaderReplace { request: true, .. }
    ));
    assert!(matches!(
        report.effective.logging[0].destination,
        LogDestination::Stdio(_)
    ));
    assert_eq!(report.effective.dns_nameservers[0].addresses.len(), 1);
    assert!(report.decision_ledger.decisions.iter().all(|decision| {
        !matches!(
            decision.outcome,
            DecisionOutcome::Classified {
                semantics: DirectiveSemantics::Unknown,
                ..
            }
        )
    }));
}

#[test]
fn duplicate_acl_and_authentication_resolution_rules_are_explicit() {
    let directory = tempdir().expect("temp directory");
    let root = directory.path().join("squid.conf");
    fs::write(
        &root,
        b"acl mixed src 192.0.2.0/24\n\
          acl mixed port 3128\n\
          auth_param basic realm First synthetic realm\n\
          auth_param basic realm Last synthetic realm\n\
          http_access deny mixed\n",
    )
    .expect("synthetic source");

    let report = import(&root);
    assert_eq!(report.effective.acls.len(), 1);
    assert_eq!(report.effective.acls[0].acl_type, AclType::Source);
    let second = &report.decision_ledger.decisions[1];
    assert!(matches!(
        second.outcome,
        DecisionOutcome::Classified {
            resolution: DirectiveResolution::MergeSameName,
            activation: Activation::Blocked(SemanticBlockerKind::ConflictingAclType),
            ..
        }
    ));
    let scheme = &report.effective.authentication_schemes[0];
    assert_eq!(scheme.parameters.len(), 2);
    assert_eq!(
        scheme.realm.expect("last realm fact").span,
        match report.effective.authentication[1].value {
            AuthenticationValue::Realm(secret) => secret.span,
            _ => panic!("expected redacted realm"),
        }
    );
}
