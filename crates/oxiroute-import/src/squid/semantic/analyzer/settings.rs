#[allow(clippy::wildcard_imports)]
use super::*;

impl Analyzer<'_> {
    pub(super) fn access_log(&mut self, expanded: &ExpandedDirective) {
        let Some((destination, rest)) = expanded.directive.arguments.split_first() else {
            self.invalid(
                expanded,
                DirectiveFamily::Logging,
                DirectiveSemantics::AccessLogging,
                "Squid access_log requires a destination",
            );
            return;
        };
        let destination = parse_log_destination(destination);
        self.effective.logging.push(LoggingDirective {
            origin: origin(expanded),
            destination,
            format: rest.first().map(Into::into),
        });
        self.record(
            expanded,
            DirectiveFamily::Logging,
            DirectiveSemantics::AccessLogging,
            Activation::Blocked(SemanticBlockerKind::AccessLoggingPolicy),
        );
    }

    pub(super) fn dns_nameservers(&mut self, expanded: &ExpandedDirective) {
        let addresses = expanded
            .directive
            .arguments
            .iter()
            .map(|value| std::str::from_utf8(&value.value).ok()?.parse().ok())
            .collect::<Option<Vec<_>>>();
        let Some(addresses) = addresses.filter(|addresses| !addresses.is_empty()) else {
            self.invalid(
                expanded,
                DirectiveFamily::Dns,
                DirectiveSemantics::DnsNameservers,
                "Squid dns_nameservers requires valid IP addresses",
            );
            return;
        };
        self.effective.dns_nameservers.push(DnsNameservers {
            origin: origin(expanded),
            addresses,
        });
        self.record(
            expanded,
            DirectiveFamily::Dns,
            DirectiveSemantics::DnsNameservers,
            Activation::Blocked(SemanticBlockerKind::ResolverPolicy),
        );
    }

    pub(super) fn forwarded_for(&mut self, expanded: &ExpandedDirective) {
        let [value] = expanded.directive.arguments.as_slice() else {
            self.invalid(
                expanded,
                DirectiveFamily::Privacy,
                DirectiveSemantics::ForwardedFor,
                "Squid forwarded_for requires one mode",
            );
            return;
        };
        let mode = match value.value.as_slice() {
            b"on" => ForwardedForMode::On,
            b"off" => ForwardedForMode::Off,
            b"transparent" => ForwardedForMode::Transparent,
            b"delete" => ForwardedForMode::Delete,
            b"truncate" => ForwardedForMode::Truncate,
            _ => {
                self.invalid(
                    expanded,
                    DirectiveFamily::Privacy,
                    DirectiveSemantics::ForwardedFor,
                    "Squid forwarded_for mode is invalid",
                );
                return;
            }
        };
        self.effective.privacy.push(PrivacyDirective::ForwardedFor {
            origin: origin(expanded),
            mode,
        });
        self.record(
            expanded,
            DirectiveFamily::Privacy,
            DirectiveSemantics::ForwardedFor,
            Activation::Blocked(SemanticBlockerKind::ForwardedForPolicy),
        );
    }

    pub(super) fn via(&mut self, expanded: &ExpandedDirective) {
        let [value] = expanded.directive.arguments.as_slice() else {
            self.invalid(
                expanded,
                DirectiveFamily::Privacy,
                DirectiveSemantics::Via,
                "Squid via requires on or off",
            );
            return;
        };
        let Some(enabled) = bytes::boolean(&value.value) else {
            self.invalid(
                expanded,
                DirectiveFamily::Privacy,
                DirectiveSemantics::Via,
                "Squid via requires on or off",
            );
            return;
        };
        self.effective.privacy.push(PrivacyDirective::Via {
            origin: origin(expanded),
            enabled,
        });
        self.record(
            expanded,
            DirectiveFamily::Privacy,
            DirectiveSemantics::Via,
            Activation::Blocked(SemanticBlockerKind::ViaPolicy),
        );
    }

    pub(super) fn header_replace(&mut self, expanded: &ExpandedDirective) {
        let [name, replacement @ ..] = expanded.directive.arguments.as_slice() else {
            self.invalid(
                expanded,
                DirectiveFamily::Privacy,
                DirectiveSemantics::HeaderPrivacy,
                "Squid header replacement requires a header name",
            );
            return;
        };
        self.effective
            .privacy
            .push(PrivacyDirective::HeaderReplace {
                origin: origin(expanded),
                request: expanded.directive.name.value == b"request_header_replace",
                name: name.into(),
                replacement: replacement.iter().map(Into::into).collect(),
            });
        self.record(
            expanded,
            DirectiveFamily::Privacy,
            DirectiveSemantics::HeaderPrivacy,
            Activation::Blocked(SemanticBlockerKind::HeaderPrivacyPolicy),
        );
    }

    pub(super) fn cache(&mut self, expanded: &ExpandedDirective) {
        let directive = if expanded.directive.name.value == b"cache_mem" {
            let [value, unit] = expanded.directive.arguments.as_slice() else {
                self.invalid(
                    expanded,
                    DirectiveFamily::CachePolicy,
                    DirectiveSemantics::CacheSetting,
                    "Squid cache_mem requires a value and unit",
                );
                return;
            };
            let Some(bytes) = bytes::byte_size(&value.value, &unit.value) else {
                self.invalid(
                    expanded,
                    DirectiveFamily::CachePolicy,
                    DirectiveSemantics::CacheSetting,
                    "Squid cache_mem value is invalid",
                );
                return;
            };
            CacheDirective::MemoryBytes {
                origin: origin(expanded),
                bytes,
            }
        } else if let [value] = expanded.directive.arguments.as_slice() {
            if let Some(enabled) = bytes::boolean(&value.value) {
                CacheDirective::Toggle {
                    origin: origin(expanded),
                    name: expanded.directive.name.value.clone(),
                    enabled,
                }
            } else {
                CacheDirective::Scalar {
                    origin: origin(expanded),
                    name: expanded.directive.name.value.clone(),
                    values: vec![value.into()],
                }
            }
        } else {
            CacheDirective::Scalar {
                origin: origin(expanded),
                name: expanded.directive.name.value.clone(),
                values: expanded
                    .directive
                    .arguments
                    .iter()
                    .map(Into::into)
                    .collect(),
            }
        };
        self.effective.cache_policy.push(directive);
        self.record(
            expanded,
            DirectiveFamily::CachePolicy,
            DirectiveSemantics::CacheSetting,
            Activation::Blocked(SemanticBlockerKind::CachePolicy),
        );
    }

    pub(super) fn storage(&mut self, expanded: &ExpandedDirective) {
        let directive = if expanded.directive.name.value == b"cache_dir" {
            let [storage_type, path, size, level_one, level_two, options @ ..] =
                expanded.directive.arguments.as_slice()
            else {
                self.invalid(
                    expanded,
                    DirectiveFamily::Storage,
                    DirectiveSemantics::StorageSetting,
                    "Squid cache_dir requires type, path, size, and directory levels",
                );
                return;
            };
            let (Some(size_mib), Some(level_one), Some(level_two)) = (
                bytes::unsigned(&size.value),
                bytes::unsigned(&level_one.value),
                bytes::unsigned(&level_two.value),
            ) else {
                self.invalid(
                    expanded,
                    DirectiveFamily::Storage,
                    DirectiveSemantics::StorageSetting,
                    "Squid cache_dir numeric value is invalid",
                );
                return;
            };
            StorageDirective::CacheDir {
                origin: origin(expanded),
                storage_type: storage_type.into(),
                path: path.into(),
                size_mib,
                level_one,
                level_two,
                options: options.iter().map(Into::into).collect(),
            }
        } else {
            StorageDirective::Opaque {
                origin: origin(expanded),
                name: expanded.directive.name.value.clone(),
                argument_count: expanded.directive.arguments.len(),
            }
        };
        self.effective.storage.push(directive);
        self.record(
            expanded,
            DirectiveFamily::Storage,
            DirectiveSemantics::StorageSetting,
            Activation::Blocked(SemanticBlockerKind::StoragePolicy),
        );
    }

    pub(super) fn process(&mut self, expanded: &ExpandedDirective) {
        let directive = if expanded.directive.name.value == b"coredump_dir" {
            let [path] = expanded.directive.arguments.as_slice() else {
                self.invalid(
                    expanded,
                    DirectiveFamily::Process,
                    DirectiveSemantics::CoreDumpDirectory,
                    "Squid coredump_dir requires one path",
                );
                return;
            };
            ProcessDirective::CoreDumpDirectory {
                origin: origin(expanded),
                path: path.into(),
            }
        } else {
            ProcessDirective::Opaque {
                origin: origin(expanded),
                name: expanded.directive.name.value.clone(),
                argument_count: expanded.directive.arguments.len(),
            }
        };
        let semantics = if expanded.directive.name.value == b"coredump_dir" {
            DirectiveSemantics::CoreDumpDirectory
        } else {
            DirectiveSemantics::ProcessSetting
        };
        self.effective.process.push(directive);
        self.record(
            expanded,
            DirectiveFamily::Process,
            semantics,
            Activation::Externalized,
        );
    }

    pub(super) fn logging_control(&mut self, expanded: &ExpandedDirective) {
        self.record(
            expanded,
            DirectiveFamily::Logging,
            DirectiveSemantics::LoggingSetting,
            Activation::Blocked(SemanticBlockerKind::LoggingPolicy),
        );
    }

    pub(super) fn dns_control(&mut self, expanded: &ExpandedDirective) {
        self.effective.dns_controls.push(OpaqueDirective {
            origin: origin(expanded),
            name: expanded.directive.name.value.clone(),
            argument_count: expanded.directive.arguments.len(),
            secret: None,
        });
        self.record(
            expanded,
            DirectiveFamily::Dns,
            DirectiveSemantics::DnsSetting,
            Activation::Blocked(SemanticBlockerKind::ResolverPolicy),
        );
    }
}
