use oxiroute_config::{
    ConfigCompositionError, ConfigDraft, ValidatedConfig, compose_validated_configs,
};

/// Composes one authored source fragment with independently validated imported fragments.
///
/// Each imported fragment is valid in isolation, but merging namespaces invalidates those
/// individual proofs: names, listener binds, process fields, and references can conflict across
/// fragment boundaries. This function is the source-composition owner that converts those proofs
/// back to drafts, merges the complete namespace once, and returns only the revalidated result.
/// `source_version` is applied to imported fragments so a source-level version declaration is
/// checked at the same complete composition boundary.
///
/// # Errors
///
/// Returns an error when no fragment is supplied, process-wide fields conflict, or the complete
/// composed configuration is invalid.
pub fn compose_validated_fragments(
    authored: Option<ConfigDraft>,
    imported: Vec<ValidatedConfig>,
    source_version: u32,
) -> Result<ValidatedConfig, ConfigCompositionError> {
    let version = authored
        .as_ref()
        .map_or(source_version, |config| config.version);
    let mut fragments = Vec::with_capacity(imported.len() + usize::from(authored.is_some()));
    fragments.extend(authored);
    fragments.extend(imported.into_iter().map(|config| {
        let mut config = config.to_draft();
        config.version = version;
        config
    }));
    compose_validated_configs(fragments)
}
