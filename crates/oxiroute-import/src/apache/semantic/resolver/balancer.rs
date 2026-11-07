#[allow(clippy::wildcard_imports)]
use super::*;

impl Resolver<'_> {
    #[allow(clippy::too_many_lines)]
    pub(super) fn resolve_balancer(&mut self, directive: &ExpandedDirective) {
        let Some(argument) = directive.directive.arguments.first() else {
            self.block(
                directive,
                E_INVALID_VALUE,
                "Apache <Proxy> requires a balancer:// name",
            );
            return;
        };
        let Some(value) = word_text(argument) else {
            self.block(
                directive,
                E_INVALID_VALUE,
                "Apache balancer name is not UTF-8",
            );
            return;
        };
        let Some(name) = value.strip_prefix("balancer://") else {
            self.block(
                directive,
                E_UNSUPPORTED_DIRECTIVE,
                "only static balancer:// Proxy blocks are supported",
            );
            return;
        };
        if directive.directive.arguments.len() != 1 || name.is_empty() || has_dynamic(name) {
            self.block(
                directive,
                E_DYNAMIC_PROXY_PASS,
                "dynamic or wildcard balancer names cannot be lowered",
            );
            return;
        }
        if self.balancers.iter().any(|balancer| balancer.name == name) {
            self.block(
                directive,
                E_DUPLICATE_IDENTITY,
                "duplicate Apache balancer identity",
            );
            return;
        }
        let Some(children) = directive.children.as_deref() else {
            self.block(directive, E_INVALID_VALUE, "balancer Proxy must be a block");
            return;
        };
        let mut members = Vec::new();
        let mut identities = HashSet::new();
        self.resolved(directive.occurrence);
        for child in children {
            match lower_name(&child.directive.name.value).as_str() {
                "balancermember" => {
                    let parsed = child
                        .directive
                        .arguments
                        .first()
                        .and_then(|word| parse_member_target(word).ok());
                    let Some(member) = parsed else {
                        self.block(
                            child,
                            E_DYNAMIC_PROXY_PASS,
                            "BalancerMember must use a static HTTP or HTTPS destination",
                        );
                        continue;
                    };
                    if child.directive.arguments.len() > 2
                        || child.directive.arguments.get(1).is_some_and(|option| {
                            !word_text(option)
                                .is_some_and(|value| value.eq_ignore_ascii_case("loadfactor=1"))
                        })
                    {
                        self.block(
                            child,
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            "only equal-weight byrequests BalancerMember options are supported",
                        );
                        continue;
                    }
                    let identity = format!(
                        "{}://{}:{}",
                        member.scheme.as_str(),
                        member.host,
                        member.port
                    );
                    if !identities.insert(identity) {
                        self.block(
                            child,
                            E_DUPLICATE_IDENTITY,
                            "duplicate Apache balancer member",
                        );
                        continue;
                    }
                    self.resolved(child.occurrence);
                    members.push(EffectiveBalancerMember {
                        origin: origin(child),
                        endpoint: member.endpoint,
                        host: member.host,
                        port: member.port,
                        scheme: member.scheme,
                    });
                }
                "proxyset" => {
                    if child.directive.arguments.len() == 1
                        && child.directive.arguments[0]
                            .value
                            .eq_ignore_ascii_case(b"lbmethod=byrequests")
                    {
                        self.resolved(child.occurrence);
                    } else {
                        self.block(
                            child,
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            "Apache balancers require the equal-weight byrequests algorithm",
                        );
                    }
                }
                "balancerpersist" | "balancergrowth" | "balancerinherit" => self.block(
                    child,
                    E_DYNAMIC_BALANCER_MANAGER,
                    "Apache balancer state is dynamic and cannot be imported from static source",
                ),
                _ => self.block_subtree(
                    child,
                    E_UNSUPPORTED_DIRECTIVE,
                    "Apache balancer directive is outside the static importer subset",
                ),
            }
        }
        if members.is_empty() {
            self.block(
                directive,
                E_UNRESOLVED_REFERENCE,
                "Apache balancer requires at least one static member",
            );
        }
        self.balancers.push(EffectiveBalancer {
            origin: origin(directive),
            name: name.to_owned(),
            members,
        });
    }
}
