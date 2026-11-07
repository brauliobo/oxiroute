#[allow(clippy::wildcard_imports)]
use super::*;

/// Stable human-readable ledger used by differential fixtures and later lowering work.
#[must_use]
pub fn decision_signatures(report: &ImportReport) -> Vec<String> {
    let mut signatures = Vec::with_capacity(report.declarations.len() + report.statements.len());
    signatures.extend(report.declarations.iter().map(declaration_signature));
    signatures.extend(report.statements.iter().map(|statement| {
        let subroutine = display_bytes(&report.subroutines[statement.subroutine].name);
        format!(
            "statement:{subroutine}:{}",
            statement_signature(&statement.classification)
        )
    }));
    signatures
}

fn declaration_signature(decision: &DeclarationDecision) -> String {
    match &decision.classification {
        DeclarationClassification::Version(version) => {
            format!("declaration:version:{}", display_bytes(version.as_bytes()))
        }
        DeclarationClassification::Include {
            path,
            glob,
            resolved,
        } => format!(
            "declaration:include{}:{}:{}",
            if *glob { "+glob" } else { "" },
            display_bytes(path),
            if *resolved { "resolved" } else { "unresolved" },
        ),
        DeclarationClassification::Import { module, .. } => {
            format!("declaration:import:{}:unsupported", display_bytes(module))
        }
        DeclarationClassification::Acl { name, .. } => {
            format!("declaration:acl:{}", display_bytes(name))
        }
        DeclarationClassification::Probe { name, .. } => {
            format!("declaration:probe:{}", display_bytes(name))
        }
        DeclarationClassification::Backend { name, .. } => {
            format!("declaration:backend:{}", display_bytes(name))
        }
        DeclarationClassification::Director { name, .. } => {
            format!("declaration:director:{}", display_bytes(name))
        }
        DeclarationClassification::Subroutine { name, .. } => {
            format!("declaration:subroutine:{}", display_bytes(name))
        }
        DeclarationClassification::Unsupported => "declaration:unsupported".to_owned(),
    }
}

fn statement_signature(classification: &StatementClassification) -> &'static str {
    match classification {
        StatementClassification::Conditional(_) => "condition",
        StatementClassification::CacheDecision(FlowAction::Lookup) => "lookup",
        StatementClassification::CacheDecision(FlowAction::Hash) => "hash",
        StatementClassification::CacheDecision(FlowAction::Pass) => "pass",
        StatementClassification::CacheDecision(FlowAction::Pipe) => "pipe",
        StatementClassification::CacheDecision(FlowAction::Miss) => "miss",
        StatementClassification::CacheDecision(FlowAction::Deliver) => "deliver",
        StatementClassification::CacheDecision(_) => "flow",
        StatementClassification::CacheLifetime(CacheLifetime {
            field: CacheLifetimeField::Ttl,
            ..
        }) => "ttl",
        StatementClassification::CacheLifetime(CacheLifetime {
            field: CacheLifetimeField::Grace,
            ..
        }) => "grace",
        StatementClassification::CacheLifetime(CacheLifetime {
            field: CacheLifetimeField::Keep,
            ..
        }) => "keep",
        StatementClassification::CacheFlag(CacheFlag::HitForPass { .. }) => "hit-for-pass",
        StatementClassification::CacheFlag(_) => "cache-flag",
        StatementClassification::BackendSelection(_) => "backend-selection",
        StatementClassification::HeaderMutation(mutation) if mutation.cookie => "cookie-mutation",
        StatementClassification::HeaderMutation(_) => "header-mutation",
        StatementClassification::Hash(_) => "hash-input",
        StatementClassification::Response(ResponseAction::Redirect { .. }) => "redirect",
        StatementClassification::Response(ResponseAction::Synth { .. }) => "synth",
        StatementClassification::Response(ResponseAction::SyntheticBody(_)) => "synthetic-body",
        StatementClassification::Invalidation(Invalidation::Ban(_)) => "ban",
        StatementClassification::Invalidation(Invalidation::Purge) => "purge",
        StatementClassification::Feature(FeatureBehavior::Esi { .. }) => "esi",
        StatementClassification::Feature(FeatureBehavior::Compression { .. }) => "compression",
        StatementClassification::NewDirector { .. } => "new-director",
        StatementClassification::DirectorMethod { .. } => "director-method",
        StatementClassification::SubroutineCall { .. } => "subroutine-call",
        StatementClassification::Dynamic(_) => "dynamic",
        StatementClassification::Unsupported(_) => "unsupported",
        StatementClassification::Invalid => "invalid",
    }
}

fn display_bytes(value: &[u8]) -> String {
    String::from_utf8_lossy(value).into_owned()
}
