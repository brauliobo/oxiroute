#[allow(clippy::wildcard_imports)]
use super::*;

impl Analyzer<'_> {
    pub(super) fn auth_param(&mut self, expanded: &ExpandedDirective) {
        let [scheme, setting, values @ ..] = expanded.directive.arguments.as_slice() else {
            self.invalid(
                expanded,
                DirectiveFamily::Authentication,
                DirectiveSemantics::AuthenticationSetting,
                "Squid auth_param requires a scheme and setting",
            );
            return;
        };
        let Some((setting, semantics, value)) = self.parse_auth_value(expanded, setting, values)
        else {
            return;
        };
        self.effective.authentication.push(AuthenticationParameter {
            origin: origin(expanded),
            scheme: scheme.into(),
            setting,
            value,
        });
        self.record(
            expanded,
            DirectiveFamily::Authentication,
            semantics,
            Activation::Blocked(SemanticBlockerKind::ProxyAuthentication),
        );
    }

    fn parse_auth_value(
        &mut self,
        expanded: &ExpandedDirective,
        setting: &Word,
        values: &[Word],
    ) -> Option<(
        AuthenticationSetting,
        DirectiveSemantics,
        AuthenticationValue,
    )> {
        Some(match setting.value.as_slice() {
            b"program" if !values.is_empty() => (
                AuthenticationSetting::Program,
                DirectiveSemantics::AuthenticationHelper,
                AuthenticationValue::Helper(SecretFact {
                    kind: SecretKind::AuthenticationHelper,
                    span: bytes::words_span(values).expect("program values are nonempty"),
                }),
            ),
            b"realm" if !values.is_empty() => (
                AuthenticationSetting::Realm,
                DirectiveSemantics::AuthenticationRealm,
                AuthenticationValue::Realm(SecretFact {
                    kind: SecretKind::AuthenticationRealm,
                    span: bytes::words_span(values).expect("realm values are nonempty"),
                }),
            ),
            b"credentialsttl" => {
                let [value, unit] = values else {
                    self.invalid(
                        expanded,
                        DirectiveFamily::Authentication,
                        DirectiveSemantics::AuthenticationCredentialTtl,
                        "Squid auth credentialsttl requires a value and unit",
                    );
                    return None;
                };
                let Some(duration) = bytes::duration(&value.value, &unit.value) else {
                    self.invalid(
                        expanded,
                        DirectiveFamily::Authentication,
                        DirectiveSemantics::AuthenticationCredentialTtl,
                        "Squid auth credentialsttl is invalid",
                    );
                    return None;
                };
                (
                    AuthenticationSetting::CredentialTtl,
                    DirectiveSemantics::AuthenticationCredentialTtl,
                    AuthenticationValue::Duration(duration),
                )
            }
            b"children" | b"concurrency" if values.len() == 1 => {
                let Some(count) = bytes::unsigned(&values[0].value) else {
                    self.invalid(
                        expanded,
                        DirectiveFamily::Authentication,
                        DirectiveSemantics::AuthenticationSetting,
                        "Squid auth count is invalid",
                    );
                    return None;
                };
                let setting = if setting.value == b"children" {
                    AuthenticationSetting::Children
                } else {
                    AuthenticationSetting::Concurrency
                };
                (
                    setting,
                    DirectiveSemantics::AuthenticationSetting,
                    AuthenticationValue::Count(count),
                )
            }
            b"casesensitive" if values.len() == 1 => {
                let Some(value) = bytes::boolean(&values[0].value) else {
                    self.invalid(
                        expanded,
                        DirectiveFamily::Authentication,
                        DirectiveSemantics::AuthenticationSetting,
                        "Squid auth casesensitive value is invalid",
                    );
                    return None;
                };
                (
                    AuthenticationSetting::CaseSensitive,
                    DirectiveSemantics::AuthenticationSetting,
                    AuthenticationValue::Boolean(value),
                )
            }
            _ => (
                AuthenticationSetting::Other,
                DirectiveSemantics::AuthenticationSetting,
                AuthenticationValue::Opaque {
                    argument_count: values.len(),
                },
            ),
        })
    }

    pub(super) fn authentication_control(&mut self, expanded: &ExpandedDirective) {
        self.effective
            .authentication_controls
            .push(OpaqueDirective {
                origin: origin(expanded),
                name: expanded.directive.name.value.clone(),
                argument_count: expanded.directive.arguments.len(),
                secret: Some(SecretFact {
                    kind: SecretKind::AuthenticationHelper,
                    span: bytes::words_span(&expanded.directive.arguments)
                        .unwrap_or(expanded.directive.span),
                }),
            });
        self.record(
            expanded,
            DirectiveFamily::Authentication,
            DirectiveSemantics::AuthenticationSetting,
            Activation::Blocked(SemanticBlockerKind::ProxyAuthentication),
        );
    }

    pub(super) fn resolve_authentication(&mut self) {
        let mut indexes = HashMap::<Vec<u8>, usize>::new();
        for parameter in &self.effective.authentication {
            let index = *indexes
                .entry(parameter.scheme.value.clone())
                .or_insert_with(|| {
                    self.effective
                        .authentication_schemes
                        .push(AuthenticationScheme {
                            scheme: parameter.scheme.value.clone(),
                            parameters: Vec::new(),
                            program: None,
                            realm: None,
                            basic_program: None,
                            realm_value: None,
                            credential_ttl: None,
                            case_sensitive: None,
                            unsupported_settings: false,
                        });
                    self.effective.authentication_schemes.len() - 1
                });
            let scheme = &mut self.effective.authentication_schemes[index];
            scheme.parameters.push(parameter.origin.occurrence);
            match parameter.value {
                AuthenticationValue::Helper(secret) => {
                    scheme.program = Some(secret);
                    if parameter.setting == AuthenticationSetting::Program {
                        let arguments = self
                            .graph
                            .expanded_directives
                            .get(parameter.origin.occurrence.get())
                            .map(|expanded| {
                                expanded
                                    .directive
                                    .arguments
                                    .iter()
                                    .skip(2)
                                    .map(|word| word.value.clone())
                                    .collect()
                            })
                            .unwrap_or_default();
                        scheme.basic_program = Some(AuthenticationHelper { secret, arguments });
                    }
                }
                AuthenticationValue::Realm(secret) => {
                    scheme.realm = Some(secret);
                    let value = self
                        .graph
                        .expanded_directives
                        .get(parameter.origin.occurrence.get())
                        .map(|expanded| {
                            expanded
                                .directive
                                .arguments
                                .iter()
                                .skip(2)
                                .map(|word| word.value.as_slice())
                                .collect::<Vec<_>>()
                                .join(&b' ')
                        })
                        .unwrap_or_default();
                    scheme.realm_value = Some(AuthenticationRealm { secret, value });
                }
                AuthenticationValue::Duration(duration)
                    if parameter.setting == AuthenticationSetting::CredentialTtl =>
                {
                    scheme.credential_ttl = Some(duration);
                }
                AuthenticationValue::Boolean(value)
                    if parameter.setting == AuthenticationSetting::CaseSensitive =>
                {
                    scheme.case_sensitive = Some(value);
                }
                _ => scheme.unsupported_settings = true,
            }
        }
    }
}
