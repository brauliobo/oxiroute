#[allow(clippy::wildcard_imports)]
use super::*;

impl Resolver<'_> {
    pub(super) fn check_listener_references(&mut self) {
        for virtual_host in &mut self.virtual_hosts {
            if !virtual_host.addresses.iter().all(|address| {
                self.listens
                    .iter()
                    .any(|listen| socket_addresses_overlap(listen.address, *address))
            }) {
                self.diagnostics.push(
                    Diagnostic::new(
                        E_UNRESOLVED_REFERENCE,
                        Severity::Error,
                        DiagnosticStage::Resolve,
                        "Apache VirtualHost has no matching explicit Listen endpoint",
                    )
                    .with_primary_span(virtual_host.origin.span)
                    .with_include_stack(
                        virtual_host
                            .origin
                            .provenance
                            .include_stack
                            .iter()
                            .map(|frame| frame.directive_span),
                    ),
                );
                virtual_host.blocked.push(E_UNRESOLVED_REFERENCE);
            }
        }
        for listen in &self.listens {
            if !self.virtual_hosts.iter().any(|virtual_host| {
                virtual_host
                    .addresses
                    .iter()
                    .any(|address| socket_addresses_overlap(*address, listen.address))
            }) {
                self.diagnostics.push(
                    Diagnostic::new(
                        E_UNRESOLVED_REFERENCE,
                        Severity::Error,
                        DiagnosticStage::Resolve,
                        "Apache Listen endpoint has no imported VirtualHost",
                    )
                    .with_primary_span(listen.origin.span)
                    .with_include_stack(
                        listen
                            .origin
                            .provenance
                            .include_stack
                            .iter()
                            .map(|frame| frame.directive_span),
                    ),
                );
            }
        }
    }

    pub(super) fn check_virtual_host_identities(&mut self) {
        for first in 0..self.virtual_hosts.len() {
            for second in first + 1..self.virtual_hosts.len() {
                if !self.virtual_hosts[first]
                    .addresses
                    .iter()
                    .any(|first_address| {
                        self.virtual_hosts[second]
                            .addresses
                            .iter()
                            .any(|second_address| {
                                socket_addresses_overlap(*first_address, *second_address)
                            })
                    })
                {
                    continue;
                }
                let first_names = self.virtual_hosts[first].names.clone();
                let second_names = self.virtual_hosts[second].names.clone();
                for first_name in first_names {
                    for second_name in &second_names {
                        if selector_key(&first_name.selector) != selector_key(&second_name.selector)
                        {
                            continue;
                        }
                        let first_origin = self.virtual_hosts[first].origin.clone();
                        let origin = second_name.origin.clone();
                        let diagnostic = Diagnostic::new(
                            E_AMBIGUOUS_VHOST,
                            Severity::Error,
                            DiagnosticStage::Resolve,
                            "Apache virtual hosts have an ambiguous listener and host identity",
                        )
                        .with_primary_span(origin.span)
                        .with_related_span(first_origin.span, "first virtual host claim is here")
                        .with_include_stack(
                            origin
                                .provenance
                                .include_stack
                                .iter()
                                .map(|frame| frame.directive_span),
                        );
                        self.diagnostics.push(diagnostic);
                        self.virtual_hosts[second].blocked.push(E_AMBIGUOUS_VHOST);
                        self.virtual_hosts[first].blocked.push(E_AMBIGUOUS_VHOST);
                    }
                }
            }
        }
    }

    pub(super) fn classify_remaining(&mut self) {
        for occurrence in &self.graph.expanded_occurrences {
            self.dispositions
                .entry(occurrence.id)
                .or_insert(OccurrenceDisposition::Structural);
        }
    }

    pub(super) fn subtree_occurrences(
        directive: &ExpandedDirective,
    ) -> Box<dyn Iterator<Item = OccurrenceId> + '_> {
        let mut occurrences = vec![directive.occurrence];
        if let Some(children) = &directive.children {
            occurrences.extend(children.iter().flat_map(|child| {
                let mut values = vec![child.occurrence];
                if let Some(children) = &child.children {
                    values.extend(children.iter().map(|nested| nested.occurrence));
                }
                values
            }));
        }
        Box::new(occurrences.into_iter())
    }

    pub(super) fn resolved(&mut self, occurrence: OccurrenceId) {
        self.dispositions
            .entry(occurrence)
            .or_insert(OccurrenceDisposition::Resolved);
    }

    pub(super) fn structural(&mut self, occurrence: OccurrenceId) {
        self.dispositions
            .entry(occurrence)
            .or_insert(OccurrenceDisposition::Structural);
    }

    pub(super) fn block(
        &mut self,
        directive: &ExpandedDirective,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) {
        let already_blocked = matches!(
            self.dispositions.get(&directive.occurrence),
            Some(OccurrenceDisposition::Blocking(_))
        );
        self.dispositions
            .insert(directive.occurrence, OccurrenceDisposition::Blocking(code));
        if already_blocked {
            return;
        }
        self.diagnostics.push(
            Diagnostic::new(code, Severity::Error, DiagnosticStage::Resolve, message)
                .with_primary_span(directive.directive.span)
                .with_include_stack(
                    directive
                        .provenance
                        .include_stack
                        .iter()
                        .map(|frame| frame.directive_span),
                ),
        );
    }

    pub(super) fn block_subtree(
        &mut self,
        directive: &ExpandedDirective,
        code: DiagnosticCode,
        message: &'static str,
    ) {
        self.block(directive, code, message);
        if let Some(children) = &directive.children {
            for child in children {
                self.block_subtree(child, code, message);
            }
        }
    }
}
