#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn assert_test_reference(evidence: &EvidenceEntry, reference: &str) {
    let (relative_path, function_name) = reference_parts(&evidence.id, reference);
    assert!(
        relative_path.contains("/tests/")
            && Path::new(relative_path)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("rs")),
        "{} evidence must reference a Rust test file: {reference}",
        evidence.id
    );
    let source = read_source(relative_path);
    let syntax = syn::parse_file(&source)
        .unwrap_or_else(|error| panic!("parse evidence {relative_path}: {error}"));
    let matches = syntax
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Fn(function) if function.sig.ident == function_name => Some(function),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "{} evidence does not resolve exactly one function: {reference}",
        evidence.id
    );
    assert!(
        matches[0].attrs.iter().any(is_test_attribute),
        "{} evidence is stale because the function is not a test: {reference}",
        evidence.id
    );
}

pub(super) fn is_test_attribute(attribute: &Attribute) -> bool {
    attribute
        .path()
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "test")
}

pub(super) fn validate_reference<E>(
    manifest: &DirectiveManifest<E>,
    product: &str,
    kind: &str,
    value: &str,
    documentation: &str,
) {
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.product, product);
    assert_eq!(manifest.reference.kind, kind);
    assert_eq!(manifest.reference.value, value);
    assert_eq!(manifest.reference.documentation, documentation);
    assert!(workspace_path(documentation).is_file());
}

pub(super) fn validate_directive_forms(entries: &[DirectiveForm]) {
    for entry in entries {
        assert!(!entry.id.is_empty());
        assert!(!entry.key.is_empty(), "{} has an empty key", entry.id);
        assert!(!entry.form.is_empty(), "{} has an empty form", entry.id);
        assert_nonempty_unique(&entry.contexts, &entry.id, "contexts");
        validate_terminal_target(&entry.id, entry.disposition, &entry.target);
        validate_test_categories(&entry.id, &entry.required_tests);
    }
}

pub(super) fn validate_squid_families(families: &[SquidFamilyManifest]) {
    for family in families {
        assert!(!family.id.is_empty());
        assert!(!family.name.is_empty(), "{} has an empty name", family.id);
        assert!(
            !family.rationale.is_empty(),
            "{} has no rationale",
            family.id
        );
        assert_nonempty_unique(
            &family.current_evidence,
            &family.id,
            "current evidence references",
        );
        validate_test_categories(&family.id, &family.required_tests);
    }
}

pub(super) fn validate_squid_directive_forms(
    families: &[SquidFamilyManifest],
    entries: &[SquidDirectiveForm],
) {
    let family_ids = families
        .iter()
        .map(|family| family.id.as_str())
        .collect::<BTreeSet<_>>();
    for entry in entries {
        assert!(!entry.id.is_empty());
        assert!(!entry.key.is_empty(), "{} has an empty key", entry.id);
        assert!(!entry.family.is_empty(), "{} has no family", entry.id);
        assert!(!entry.form.is_empty(), "{} has an empty form", entry.id);
        assert!(
            family_ids.contains(entry.family.as_str()),
            "{} references an absent family {}",
            entry.id,
            entry.family
        );
        assert_nonempty_unique(&entry.contexts, &entry.id, "contexts");
        assert!(!entry.rationale.is_empty(), "{} has no rationale", entry.id);
        assert_nonempty_unique(
            &entry.current_evidence,
            &entry.id,
            "current evidence references",
        );
        validate_terminal_target(&entry.id, entry.disposition, &entry.target);
        validate_test_categories(&entry.id, &entry.required_tests);
        if entry.status == CapabilityStatus::Compatible {
            assert!(
                entry
                    .required_tests
                    .iter()
                    .any(|test| test == "integration"),
                "{} compatible form lacks runtime integration coverage",
                entry.id
            );
            assert!(
                entry.required_tests.iter().any(|test| test == "failure"),
                "{} compatible form lacks failure coverage",
                entry.id
            );
            assert!(
                matches!(
                    entry.disposition,
                    Disposition::Lowered | Disposition::Classified
                ),
                "{} compatible form is not lowered or classified",
                entry.id
            );
        }
        if matches!(
            entry.status,
            CapabilityStatus::Unsupported
                | CapabilityStatus::Obsolete
                | CapabilityStatus::NotPlanned
        ) {
            assert!(
                matches!(
                    entry.disposition,
                    Disposition::Blocked | Disposition::Externalized
                ),
                "{} open form is not blocked or externalized",
                entry.id
            );
        }
    }
}

pub(super) fn assert_rtmp_metadata(entry: &RtmpDirectiveEntry, spec: &DirectiveSpec) {
    assert_eq!(
        entry.id,
        format!("directive.nginx-rtmp.{}", spec.name.replace('_', "-"))
    );
    assert_eq!(entry.contexts, rtmp_contexts(spec.contexts));
    assert_eq!(entry.min_args, spec.min_args);
    assert_eq!(entry.max_args, spec.max_args);
    let (value_kind, values, relay_kind) = rtmp_value_kind(spec.value_kind);
    assert_eq!(entry.value_kind, value_kind, "{} value kind", spec.name);
    assert_eq!(entry.values, values, "{} closed values", spec.name);
    assert_eq!(entry.relay_kind, relay_kind, "{} relay kind", spec.name);
    assert_eq!(entry.default.as_deref(), spec.default);
    assert_eq!(entry.repeatable, spec.repeatable);
    assert_eq!(
        entry.runtime_status,
        rtmp_runtime_status(spec.runtime_support)
    );
}

pub(super) fn validate_rtmp_terminal_decision(entry: &RtmpDirectiveEntry) {
    let (disposition, target) = match entry.runtime_status {
        RtmpRuntimeStatus::SourceBug => (Disposition::Blocked, "import.blocking_diagnostic"),
        RtmpRuntimeStatus::PlatformLimited => (Disposition::Externalized, "deployment"),
        RtmpRuntimeStatus::ParsedNotEnforced
        | RtmpRuntimeStatus::SourceNoOp
        | RtmpRuntimeStatus::Deprecated => (Disposition::Classified, "rtmp.directive_registry"),
    };
    assert_eq!(entry.disposition, disposition, "{} disposition", entry.id);
    assert_eq!(entry.target, target, "{} target", entry.id);
    validate_terminal_target(&entry.id, entry.disposition, &entry.target);
}

pub(super) fn validate_rtmp_reference(manifest: &RtmpDirectiveManifest) {
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.product, "nginx-rtmp");
    assert_eq!(manifest.reference.kind, "release_and_git_checkout");
    assert_eq!(manifest.reference.value, "1.1.4+git.6c7719d");
    assert_eq!(manifest.reference.documentation, "docs/RTMP_SPEC.md");
    assert!(workspace_path(&manifest.reference.documentation).is_file());
}

#[allow(clippy::too_many_lines)]
pub(super) fn validate_rtmp_import_forms(manifest: &RtmpDirectiveManifest) {
    let expected = [
        "import.nginx-rtmp.allow.bounded",
        "import.nginx-rtmp.deny.bounded",
        "import.nginx-rtmp.push.static",
        "import.nginx-rtmp.pull.unrepresented",
        "import.nginx-rtmp.access-log.disabled",
        "import.nginx-rtmp.access-log.combined-file",
        "import.nginx-rtmp.access-log.unsupported",
        "import.nginx-rtmp.hls.bounded",
        "import.nginx-rtmp.hls.invalid",
        "import.nginx-rtmp.auto-push.bounded",
        "import.nginx-rtmp.auto-push.invalid",
        "import.nginx-rtmp.exec.unsupported",
        "import.nginx-rtmp.max-connections.application",
        "import.nginx-rtmp.max-message.bounded",
        "import.nginx-rtmp.ack-window.bounded",
        "import.nginx-rtmp.exec-publisher.typed",
        "import.nginx-rtmp.exec-alias.typed",
        "import.nginx-rtmp.exec-publish.typed",
        "import.nginx-rtmp.exec-publish-done.typed",
        "import.nginx-rtmp.respawn.bounded",
        "import.nginx-rtmp.record.off",
        "import.nginx-rtmp.record.all",
        "import.nginx-rtmp.record.all-manual",
        "import.nginx-rtmp.record.media-mask",
        "import.nginx-rtmp.record.inexact",
        "import.nginx-rtmp.record-path.secure-absolute",
        "import.nginx-rtmp.record-path.invalid",
        "import.nginx-rtmp.record-suffix.exact",
        "import.nginx-rtmp.record-suffix.inexact",
        "import.nginx-rtmp.record-unique.flag",
        "import.nginx-rtmp.record-interval.exact",
        "import.nginx-rtmp.record-interval.invalid-or-manual",
        "import.nginx-rtmp.record-append.disabled",
        "import.nginx-rtmp.record-append.enabled",
        "import.nginx-rtmp.record-lock.disabled",
        "import.nginx-rtmp.record-lock.enabled",
        "import.nginx-rtmp.record-max-size.unlimited",
        "import.nginx-rtmp.record-max-size.limited",
        "import.nginx-rtmp.record-max-frames.unlimited",
        "import.nginx-rtmp.record-max-frames.limited",
        "import.nginx-rtmp.record-notify.disabled",
        "import.nginx-rtmp.record-notify.enabled",
        "import.nginx-rtmp.recorder.named",
        "import.nginx-rtmp.recorder.invalid",
        "import.nginx-rtmp.hls-muxdelay.source-noop",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    let actual = manifest
        .import_forms
        .iter()
        .map(|form| form.id.clone())
        .collect::<BTreeSet<_>>();
    assert_set_equality("nginx-RTMP importer forms", &expected, &actual);
    assert_eq!(
        actual.len(),
        manifest.import_forms.len(),
        "duplicate nginx-RTMP importer form"
    );

    let registry_keys = manifest
        .entries
        .iter()
        .map(|entry| entry.key.as_str())
        .collect::<HashSet<_>>();
    for form in &manifest.import_forms {
        assert!(!form.form.is_empty(), "{} has an empty form", form.id);
        assert!(
            registry_keys.contains(form.key.as_str()),
            "{} references unregistered directive `{}`",
            form.id,
            form.key
        );
        assert!(!form.contexts.is_empty(), "{} has no contexts", form.id);
        assert_eq!(
            form.contexts.iter().collect::<HashSet<_>>().len(),
            form.contexts.len(),
            "{} repeats importer contexts",
            form.id
        );
        assert!(
            matches!(
                form.disposition,
                Disposition::Lowered | Disposition::Classified | Disposition::Blocked
            ),
            "{} must describe an enforced importer decision",
            form.id
        );
        validate_terminal_target(&form.id, form.disposition, &form.target);
        validate_test_categories(&form.id, &form.required_tests);
    }

    for key in [
        "push",
        "pull",
        "access_log",
        "hls",
        "rtmp_auto_push",
        "rtmp_auto_push_reconnect",
        "exec_pull",
        "record",
        "record_path",
        "record_suffix",
        "record_unique",
        "record_append",
        "record_lock",
        "record_max_size",
        "record_max_frames",
        "record_interval",
        "record_notify",
        "recorder",
        "hls_muxdelay",
    ] {
        assert!(
            manifest.import_forms.iter().any(|form| form.key == key),
            "recording directive `{key}` has no strict importer disposition"
        );
    }
}

pub(super) fn rtmp_contexts(contexts: &[DirectiveContext]) -> Vec<RtmpContext> {
    contexts
        .iter()
        .map(|context| match context {
            DirectiveContext::NginxMain => RtmpContext::NginxMain,
            DirectiveContext::RtmpMain => RtmpContext::RtmpMain,
            DirectiveContext::RtmpServer => RtmpContext::RtmpServer,
            DirectiveContext::RtmpApplication => RtmpContext::RtmpApplication,
            DirectiveContext::RtmpRecorder => RtmpContext::RtmpRecorder,
            DirectiveContext::Http => RtmpContext::Http,
        })
        .collect()
}

pub(super) fn rtmp_value_kind(
    kind: ValueKind,
) -> (RtmpValueKind, Vec<String>, Option<RtmpRelayKind>) {
    let (kind, values, relay) = match kind {
        ValueKind::AccessLog => (RtmpValueKind::AccessLog, &[][..], None),
        ValueKind::AccessRule => (RtmpValueKind::AccessRule, &[][..], None),
        ValueKind::Bitmask(values) => (RtmpValueKind::Bitmask, values, None),
        ValueKind::Block => (RtmpValueKind::Block, &[][..], None),
        ValueKind::Command => (RtmpValueKind::Command, &[][..], None),
        ValueKind::Duration => (RtmpValueKind::Duration, &[][..], None),
        ValueKind::DurationOrOff => (RtmpValueKind::DurationOrOff, &[][..], None),
        ValueKind::Enum(values) => (RtmpValueKind::Enum, values, None),
        ValueKind::Flag => (RtmpValueKind::Flag, &[][..], None),
        ValueKind::HlsVariant => (RtmpValueKind::HlsVariant, &[][..], None),
        ValueKind::Integer => (RtmpValueKind::Integer, &[][..], None),
        ValueKind::Listen => (RtmpValueKind::Listen, &[][..], None),
        ValueKind::LogFormat => (RtmpValueKind::LogFormat, &[][..], None),
        ValueKind::NamedBlock => (RtmpValueKind::NamedBlock, &[][..], None),
        ValueKind::Path => (RtmpValueKind::Path, &[][..], None),
        ValueKind::RelayTarget(RelayKind::Push) => (
            RtmpValueKind::RelayTarget,
            &[][..],
            Some(RtmpRelayKind::Push),
        ),
        ValueKind::RelayTarget(RelayKind::Pull) => (
            RtmpValueKind::RelayTarget,
            &[][..],
            Some(RtmpRelayKind::Pull),
        ),
        ValueKind::Signal => (RtmpValueKind::Signal, &[][..], None),
        ValueKind::Size => (RtmpValueKind::Size, &[][..], None),
        ValueKind::Strings => (RtmpValueKind::Strings, &[][..], None),
        ValueKind::Url => (RtmpValueKind::Url, &[][..], None),
    };
    (
        kind,
        values.iter().map(ToString::to_string).collect(),
        relay,
    )
}

pub(super) const fn rtmp_runtime_status(status: RuntimeSupport) -> RtmpRuntimeStatus {
    match status {
        RuntimeSupport::ParsedNotEnforced => RtmpRuntimeStatus::ParsedNotEnforced,
        RuntimeSupport::SourceNoOp => RtmpRuntimeStatus::SourceNoOp,
        RuntimeSupport::SourceBug => RtmpRuntimeStatus::SourceBug,
        RuntimeSupport::Deprecated => RtmpRuntimeStatus::Deprecated,
        RuntimeSupport::PlatformLimited => RtmpRuntimeStatus::PlatformLimited,
    }
}

pub(super) fn valid_rtmp_arguments(spec: &DirectiveSpec) -> Vec<&'static str> {
    let first = match spec.value_kind {
        ValueKind::AccessLog => "off",
        ValueKind::AccessRule => "all",
        ValueKind::Bitmask(values) | ValueKind::Enum(values) => values[0],
        ValueKind::Block => return Vec::new(),
        ValueKind::Command => "true",
        ValueKind::Duration | ValueKind::DurationOrOff => "1s",
        ValueKind::Flag => "on",
        ValueKind::HlsVariant => "low",
        ValueKind::Integer => "1",
        ValueKind::Listen => "1935",
        ValueKind::LogFormat => "main",
        ValueKind::NamedBlock => "name",
        ValueKind::Path => "/tmp/value",
        ValueKind::RelayTarget(_) => "rtmp://origin/live",
        ValueKind::Signal => "TERM",
        ValueKind::Size => "1M",
        ValueKind::Strings => "value",
        ValueKind::Url => "http://localhost/hook",
    };
    let mut arguments = vec![first; usize::from(spec.min_args)];
    if spec.value_kind == ValueKind::LogFormat && arguments.len() >= 2 {
        arguments[1] = "format";
    }
    arguments
}

pub(super) const fn all_rtmp_contexts() -> [DirectiveContext; 6] {
    [
        DirectiveContext::NginxMain,
        DirectiveContext::RtmpMain,
        DirectiveContext::RtmpServer,
        DirectiveContext::RtmpApplication,
        DirectiveContext::RtmpRecorder,
        DirectiveContext::Http,
    ]
}

pub(crate) fn validate_runtime_decision(id: &str, decision: &RuntimeDecision) {
    validate_terminal_target(id, decision.disposition, &decision.target);
    assert!(
        matches!(
            decision.disposition,
            Disposition::Lowered | Disposition::Classified
        ),
        "canonical entry `{id}` cannot be externalized or blocked after acceptance"
    );
}

pub(crate) fn validate_terminal_target(id: &str, disposition: Disposition, target: &str) {
    assert!(
        TARGETS.contains(&target) || ADDITIONAL_TARGETS.contains(&target),
        "coverage entry `{id}` has invalid target `{target}`"
    );
    match disposition {
        Disposition::Externalized => assert_eq!(target, "deployment", "{id}"),
        Disposition::Blocked => assert_eq!(target, "import.blocking_diagnostic", "{id}"),
        Disposition::Lowered => assert_ne!(target, "deployment", "{id}"),
        Disposition::Classified => assert_ne!(target, "import.blocking_diagnostic", "{id}"),
    }
}

pub(crate) fn validate_test_categories(id: &str, categories: &[String]) {
    assert_nonempty_unique(categories, id, "required test categories");
    for category in categories {
        assert!(
            TEST_CATEGORIES.contains(&category.as_str()),
            "coverage entry `{id}` has invalid test category `{category}`"
        );
    }
}
