use std::sync::Arc;

use super::{
    AuthoredRevision, ResolvedConfigDocument,
    external::{self, Digest},
};

/// The last resolved document, valid for exactly the disk bytes that produced it.
///
/// Resolution is a pure function of the authored bytes unless the source reads other files
/// (native references), so only documents without dependencies are retained. The files a
/// configuration refers to are read again on every lookup, because their content is part of the
/// effective revision.
#[derive(Default)]
pub(super) struct LoadCache {
    entry: Option<Entry>,
    #[cfg(test)]
    pub(super) resolutions: usize,
}

struct Entry {
    document: Arc<ResolvedConfigDocument>,
    canonical_digest: [u8; 32],
    external: Digest,
}

impl LoadCache {
    pub(super) fn get(&mut self, disk: &AuthoredRevision) -> Option<Arc<ResolvedConfigDocument>> {
        let entry = self
            .entry
            .as_mut()
            .filter(|entry| entry.document.authored_revision == *disk)?;
        let external = external::digest(&entry.document.validated_config);
        if external != entry.external {
            let mut document = ResolvedConfigDocument::clone(&entry.document);
            document.effective_revision = external::revision(entry.canonical_digest, external);
            entry.document = Arc::new(document);
            entry.external = external;
        }
        Some(Arc::clone(&entry.document))
    }

    pub(super) fn store(
        &mut self,
        document: ResolvedConfigDocument,
        canonical_digest: [u8; 32],
        external: Digest,
    ) -> Arc<ResolvedConfigDocument> {
        #[cfg(test)]
        {
            self.resolutions += 1;
        }
        let document = Arc::new(document);
        self.entry = document.dependencies.is_empty().then(|| Entry {
            document: Arc::clone(&document),
            canonical_digest,
            external,
        });
        document
    }
}
